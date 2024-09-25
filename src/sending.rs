use quinn::{Connection, ConnectionError, StoppedError, WriteError};
use serde::Serialize;
use tokio::sync::oneshot;

/// When a message is sent, it is sent asynchronously over a period.
/// This handle allows to get to know if the message was really properly sent in the end
/// or if something went wrong and what, the information is not available immediately.
pub struct SendingStateHandle {
    result_receiver: oneshot::Receiver<SendingResult>,
}

impl SendingStateHandle {
    pub(crate) fn from_result_receiver(
        result_receiver: oneshot::Receiver<SendingResult>,
    ) -> SendingStateHandle {
        SendingStateHandle { result_receiver }
    }

    /// Has the message finished being sent? If yes, was is successfully sent?
    /// The returned sending state answers these questions, and if the answer is that the message
    /// is still being sent then it gives back this handle to ask again later.
    pub fn get_state(mut self) -> SendingState {
        match self.result_receiver.try_recv() {
            Ok(result) => SendingState::Result(result),
            Err(oneshot::error::TryRecvError::Closed) => SendingState::Result(SendingResult::Error),
            Err(oneshot::error::TryRecvError::Empty) => SendingState::StillBeingSent(self),
        }
    }
}

/// Has the message finished being sent? With what [`SendingResult`]?
pub enum SendingState {
    /// The message is still in the process of being sent.
    /// It will finish being sent (or fail) later.
    /// The [`SendingStateHandle`] used to get this sending state is given back
    /// to try again later.
    StillBeingSent(SendingStateHandle),
    /// The message is not being sent anymore.
    /// It has either being successfully sent (in the case of [`SendingResult::Sent`])
    /// or its sending has failed (any other `SendingResult` variant.)
    Result(SendingResult),
}

/// Was the message successfully sent, or did it fail (and why)?
pub enum SendingResult {
    /// The message was sent with success!
    Sent,
    /// We (ourselves, not the peer) closed the connection before the message was entirely sent.
    WeClosed,
    /// The peer closed the connection (either intentionally or due to a panic/crash/shutdown)
    /// before the message was entirely received.
    PeerClosedOrDied,
    /// An error occured which prevented the sending of the message.
    Error,
}

impl SendingResult {
    /// Has the message been successfully sent?
    pub fn sent(&self) -> bool {
        matches!(self, SendingResult::Sent)
    }

    /// Has the message failed to be sent due to the connection being closed?
    ///
    /// Ignoring such situation might be appropriate.
    pub fn failed_due_to_connection_closed(&self) -> bool {
        matches!(
            self,
            SendingResult::WeClosed | SendingResult::PeerClosedOrDied
        )
    }

    /// Has the message failed to be sent due to some error
    /// (and not due to the connection being closed)?
    pub fn failed_due_to_error(&self) -> bool {
        matches!(self, SendingResult::Error)
    }

    pub(crate) fn from_result(result: Result<(), SendingError>) -> SendingResult {
        match result {
            Ok(()) => SendingResult::Sent,
            Err(SendingError::OpenUni(connection_error))
            | Err(SendingError::WriteAll(WriteError::ConnectionLost(connection_error)))
            | Err(SendingError::Stopped(StoppedError::ConnectionLost(connection_error))) => {
                SendingResult::from_connection_error(connection_error)
            }
            _ => {
                // All the other cases are cases that should not happen in principle.
                // Maybe panic or even `unreachable!()` here would be better?
                SendingResult::Error
            }
        }
    }

    fn from_connection_error(connection_error: ConnectionError) -> SendingResult {
        match connection_error {
            ConnectionError::LocallyClosed => SendingResult::WeClosed,
            ConnectionError::ApplicationClosed(_)
            | ConnectionError::ConnectionClosed(_)
            | ConnectionError::Reset
            | ConnectionError::TimedOut => SendingResult::PeerClosedOrDied,
            ConnectionError::VersionMismatch
            | ConnectionError::TransportError(_)
            | ConnectionError::CidsExhausted => SendingResult::Error,
        }
    }
}

#[derive(Debug)]
pub(crate) enum SendingError {
    OpenUni(ConnectionError),
    WriteAll(WriteError),
    Stopped(StoppedError),
}

pub(crate) async fn send_message(
    connection: &Connection,
    message: &impl Serialize,
) -> Result<(), SendingError> {
    let message_raw = rmp_serde::encode::to_vec(message).unwrap();
    let mut stream = connection.open_uni().await.map_err(SendingError::OpenUni)?;
    stream
        .write_all(&message_raw)
        .await
        .map_err(SendingError::WriteAll)?;
    stream.finish().unwrap();
    stream.stopped().await.map_err(SendingError::Stopped)?;
    Ok(())
}
