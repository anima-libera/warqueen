use std::{
    marker::PhantomData,
    net::SocketAddr,
    sync::{mpsc::Receiver, Arc, Barrier},
};

use quinn::{
    crypto::rustls::QuicClientConfig, ClientConfig, Connection, ConnectionError, Endpoint, VarInt,
};
use tokio::{runtime::Handle, sync::oneshot};

use crate::{
    async_runtime::async_runtime,
    disconnection::DisconnectionHandle,
    net_traits::{NetReceive, NetSend},
    receiving::receive_message_raw,
    sending::{send_message, SendingResult, SendingStateHandle},
    server::SERVER_NAME,
};

mod cerificate_verifier {
    use std::sync::Arc;

    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

    #[derive(Debug)]
    pub struct EveryoneIsValid(Arc<rustls::crypto::CryptoProvider>);

    impl EveryoneIsValid {
        pub fn new() -> Arc<Self> {
            Arc::new(Self(Arc::new(rustls::crypto::ring::default_provider())))
        }
    }

    impl rustls::client::danger::ServerCertVerifier for EveryoneIsValid {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
}

struct ClientNetworkingConnecting<S: NetSend, R: NetReceive> {
    connected_client_receiver: oneshot::Receiver<ConnectedOrTimeout<S, R>>,
    /// A message sent while the connection is still being established
    /// is stored here and will be sent once the connection is established.
    // Note: This is the reason why we have the `S` type on all the client-side types
    // (and it was put on the server-side types as well for symetry >w<).
    pending_sent_messages: Vec<PendingMessage<S>>,
}

enum ConnectedOrTimeout<S: NetSend, R: NetReceive> {
    Connected(ClientNetworkingConnected<S, R>),
    Timeout,
}

struct PendingMessage<S: NetSend> {
    message: S,
    result_sender: oneshot::Sender<SendingResult>,
}

struct ClientNetworkingConnected<S: NetSend, R: NetReceive> {
    async_runtime_handle: Handle,
    connection: Connection,
    endpoint: Endpoint,
    connected_event_already_polled: bool,
    receiving_receiver: Receiver<ClientEvent<R>>,
    _phantom: PhantomData<S>,
}

/// Returned by [`ClientNetworking::poll_event_from_server`].
///
/// Describes an event that happened regarding the connection to a server.
pub enum ClientEvent<R: NetReceive> {
    /// We actually established a connection with the server.
    Connected,
    /// The server sent us a message.
    Message(R),
    /// We got disconnected from the server.
    Disconnected(ClientDisconnectionDetails),
    /// We could not even establish a connection (in a reasonable amount of time).
    FailedToConnect,
}

/// Details about a disconnection event [`ClientEvent::Disconnected`].
pub enum ClientDisconnectionDetails {
    None,
    /// The server timed out (failed to react in time to stuff).
    Timeout,
}

enum ClientNetworkingEnum<S: NetSend, R: NetReceive> {
    /// The connection is still in the process of being established.
    /// When connected, we transition to the `Connected` variant.
    Connecting(ClientNetworkingConnecting<S, R>),
    Connected(ClientNetworkingConnected<S, R>),
    Disconnected(WhoClosed),
}

enum WhoClosed {
    Us,
    // Note: It seems that when the peer closes we just let the connection notice it
    // and it just works, so this is never constricted yet.
    // It might be useful later though and should be kept around.
    #[allow(unused)]
    ThePeer,
    /// We timeouted waiting for a connection with the peer to be established.
    /// The peer might as well not exist.
    ThePeerDidntEvenConnect {
        failed_to_connect_event_already_polled: bool,
    },
}

/// A connection to a server, from a client's point of view.
///
/// The actual connection is established after this is created,
/// which is notified in the form of a [`ClientEvent::Connected`].
/// Messages sent before that are all actually sent at that moment.
///
/// `S` and `R` are the message types that can be send and received respectively.
pub struct ClientNetworking<S: NetSend, R: NetReceive>(ClientNetworkingEnum<S, R>);

impl<S: NetSend, R: NetReceive> ClientNetworkingConnecting<S, R> {
    fn new(server_address: SocketAddr) -> ClientNetworkingConnecting<S, R> {
        rustls::crypto::ring::default_provider()
            .install_default()
            .unwrap();

        let async_runtime_handle = async_runtime();

        let (connected_client_sender, connected_client_receiver) = oneshot::channel();

        let async_runtime_handle_cloned = async_runtime_handle.clone();

        async_runtime_handle.spawn(async move {
            let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();

            endpoint.set_default_client_config(ClientConfig::new(Arc::new(
                QuicClientConfig::try_from(
                    rustls::ClientConfig::builder()
                        .dangerous()
                        .with_custom_certificate_verifier(
                            cerificate_verifier::EveryoneIsValid::new(),
                        )
                        .with_no_client_auth(),
                )
                .unwrap(),
            )));

            let connection_result = endpoint.connect(server_address, SERVER_NAME).unwrap().await;
            match connection_result {
                Ok(connection) => {
                    let connected_client = ClientNetworkingConnected::new(
                        async_runtime_handle_cloned,
                        connection,
                        endpoint,
                    );
                    let _ = connected_client_sender
                        .send(ConnectedOrTimeout::Connected(connected_client));
                }
                Err(ConnectionError::TimedOut) => {
                    let _ = connected_client_sender.send(ConnectedOrTimeout::Timeout);
                }
                Err(error) => panic!("{error:?}"),
            }
        });

        ClientNetworkingConnecting {
            connected_client_receiver,
            pending_sent_messages: vec![],
        }
    }

    fn connected(&mut self) -> Option<ConnectedOrTimeout<S, R>> {
        match self.connected_client_receiver.try_recv().ok()? {
            ConnectedOrTimeout::Connected(connected_client) => {
                let pending_sent_messages = std::mem::take(&mut self.pending_sent_messages);
                for pending_message in pending_sent_messages.into_iter() {
                    connected_client.send_message_to_server_with_result_sender(
                        pending_message.message,
                        pending_message.result_sender,
                    );
                }
                Some(ConnectedOrTimeout::Connected(connected_client))
            }
            ConnectedOrTimeout::Timeout => Some(ConnectedOrTimeout::Timeout),
        }
    }
}

impl<S: NetSend, R: NetReceive> ClientNetworkingConnected<S, R> {
    fn new(
        async_runtime_handle: Handle,
        connection: Connection,
        endpoint: Endpoint,
    ) -> ClientNetworkingConnected<S, R> {
        let (receiving_sender, receiving_receiver) = std::sync::mpsc::channel();

        let connection_cloned = connection.clone();
        tokio::spawn(async move {
            loop {
                match connection_cloned.accept_uni().await {
                    Ok(mut stream) => {
                        // Received a stream, that we will read until the end
                        // to get the entire message that we can then provide to the user.
                        let receiving_sender_cloned = receiving_sender.clone();
                        tokio::spawn(async move {
                            let message_raw = receive_message_raw(&mut stream).await;
                            let message: R = rmp_serde::decode::from_slice(&message_raw).unwrap();
                            let event = ClientEvent::Message(message);
                            receiving_sender_cloned.send(event).unwrap();
                        });
                    }
                    Err(ConnectionError::ApplicationClosed(_thingy)) => {
                        // TODO: Deserialize the reason from `_thingy` and put it in the event.
                        let event = ClientEvent::Disconnected(ClientDisconnectionDetails::None);
                        receiving_sender.send(event).unwrap();
                        return;
                    }
                    Err(ConnectionError::ConnectionClosed(_thingy)) => {
                        // TODO: Deserialize the reason from `_thingy` and put it in the event.
                        let event = ClientEvent::Disconnected(ClientDisconnectionDetails::None);
                        receiving_sender.send(event).unwrap();
                        return;
                    }
                    Err(ConnectionError::LocallyClosed) => {
                        // Our own side have closed the connection, let's just wrap up as expected.
                        return;
                    }
                    Err(ConnectionError::TimedOut) => {
                        let event = ClientEvent::Disconnected(ClientDisconnectionDetails::Timeout);
                        receiving_sender.send(event).unwrap();
                        return;
                    }
                    Err(error) => {
                        // TODO: Handle more errors to pass as events to the user.
                        panic!("{error}");
                    }
                }
            }
        });

        ClientNetworkingConnected {
            async_runtime_handle,
            connection,
            endpoint,
            connected_event_already_polled: false,
            receiving_receiver,
            _phantom: PhantomData,
        }
    }

    fn send_message_to_server(&self, message: S) -> SendingStateHandle {
        let connection = self.connection.clone();
        let (result_sender, result_receiver) = oneshot::channel();
        self.async_runtime_handle.spawn(async move {
            let message = message;
            let result = send_message(&connection, &message).await;
            let _ = result_sender.send(SendingResult::from_result(result));
        });
        SendingStateHandle::from_result_receiver(result_receiver)
    }

    fn send_message_to_server_with_result_sender(
        &self,
        message: S,
        result_sender: oneshot::Sender<SendingResult>,
    ) {
        let connection = self.connection.clone();
        self.async_runtime_handle.spawn(async move {
            let message = message;
            let result = send_message(&connection, &message).await;
            let _ = result_sender.send(SendingResult::from_result(result));
        });
    }

    fn poll_event_from_client(&mut self) -> Option<ClientEvent<R>> {
        if !self.connected_event_already_polled {
            self.connected_event_already_polled = true;
            Some(ClientEvent::Connected)
        } else {
            self.receiving_receiver.try_recv().ok()
        }
    }
}

impl<S: NetSend, R: NetReceive> ClientNetworking<S, R> {
    /// Connects to the server at the given address.
    pub fn new(server_address: SocketAddr) -> ClientNetworking<S, R> {
        ClientNetworking(ClientNetworkingEnum::Connecting(
            ClientNetworkingConnecting::new(server_address),
        ))
    }

    /// Transitions to the `Connected` variant if we finally established the connection.
    fn connect_if_possible(&mut self) {
        if let ClientNetworkingEnum::Connecting(connecting) = &mut self.0 {
            if let Some(connected_or_timeout) = connecting.connected() {
                match connected_or_timeout {
                    ConnectedOrTimeout::Connected(connected) => {
                        self.0 = ClientNetworkingEnum::Connected(connected);
                    }
                    ConnectedOrTimeout::Timeout => {
                        self.0 = ClientNetworkingEnum::Disconnected(
                            WhoClosed::ThePeerDidntEvenConnect {
                                failed_to_connect_event_already_polled: false,
                            },
                        );
                    }
                }
            }
        }
    }

    /// Sends the given message to the server.
    ///
    /// Takes some time, the message is sent over time.
    /// If the sending was not finished when the connection is closed then
    /// the server won't receive the message.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use serde::{Serialize, Deserialize};
    /// # use warqueen::*;
    /// #
    /// # #[derive(Serialize, Deserialize)]
    /// # enum MessageServerToClient {
    /// #     Hello,
    /// #     // ...
    /// # }
    /// # impl NetReceive for MessageServerToClient {}
    /// #
    /// #[derive(Serialize, Deserialize)]
    /// enum MessageClientToServer {
    ///     Hello,
    ///     // ...
    /// }
    /// impl NetSend for MessageClientToServer {}
    ///
    /// # let server_address = "127.0.0.1:21001".parse().unwrap();
    /// let mut client = ClientNetworking::new(server_address);
    ///
    /// client.send_message_to_server(MessageClientToServer::Hello);
    /// #
    /// # let _: ClientEvent<MessageServerToClient> =
    /// #     client.poll_event_from_server().unwrap();
    /// ```
    #[inline]
    pub fn send_message_to_server(&mut self, message: S) -> SendingStateHandle {
        self.connect_if_possible();
        match &mut self.0 {
            ClientNetworkingEnum::Connecting(connecting) => {
                let (result_sender, result_receiver) = oneshot::channel();
                connecting.pending_sent_messages.push(PendingMessage {
                    message,
                    result_sender,
                });
                SendingStateHandle::from_result_receiver(result_receiver)
            }
            ClientNetworkingEnum::Connected(connected) => connected.send_message_to_server(message),
            ClientNetworkingEnum::Disconnected(who_closed) => {
                // TODO: Error maybe?
                let (result_sender, result_receiver) = oneshot::channel();
                let _ = result_sender.send(match who_closed {
                    WhoClosed::Us => SendingResult::WeClosed,
                    WhoClosed::ThePeer => SendingResult::PeerClosedOrDied,
                    WhoClosed::ThePeerDidntEvenConnect { .. } => SendingResult::PeerClosedOrDied,
                });
                SendingStateHandle::from_result_receiver(result_receiver)
            }
        }
    }

    /// If the server has sent any new messages, returns one of them.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use serde::{Serialize, Deserialize};
    /// # use warqueen::*;
    /// #
    /// # #[derive(Serialize, Deserialize)]
    /// # enum MessageClientToServer {
    /// #     Hello,
    /// #     // ...
    /// # }
    /// # impl NetSend for MessageClientToServer {}
    /// #
    /// #[derive(Serialize, Deserialize)]
    /// enum MessageServerToClient {
    ///     Hello,
    ///     // ...
    /// }
    /// impl NetReceive for MessageServerToClient {}
    ///
    /// # let server_address = "127.0.0.1:21001".parse().unwrap();
    /// let mut client = ClientNetworking::new(server_address);
    ///
    /// loop {
    ///     while let Some(event) = client.poll_event_from_server() {
    ///         match event {
    ///             ClientEvent::Message(message) => match message {
    ///                 MessageServerToClient::Hello => { /* ... */ },
    ///                 // Handle the different possible message variants...
    ///             },
    ///             ClientEvent::Connected => {
    ///                 // The client just established a connection with the server.
    ///                 // If such event is never polled then it means we can't connect.
    ///             },
    ///             ClientEvent::Disconnected(details) => {
    ///                 // Handle the server disconnection...
    ///             },
    ///             ClientEvent::FailedToConnect => {
    ///                 // We failed to connect to the server. It could mean many things,
    ///                 // maybe the address and port we tried are wrong, etc.
    ///                 // Maybe retry or fail gracefully.
    ///             },
    ///         }
    ///     }
    /// }
    /// #
    /// # client.send_message_to_server(MessageClientToServer::Hello);
    /// ```
    pub fn poll_event_from_server(&mut self) -> Option<ClientEvent<R>> {
        self.connect_if_possible();
        match &mut self.0 {
            ClientNetworkingEnum::Connecting(_connecting) => None,
            ClientNetworkingEnum::Connected(connected) => connected.poll_event_from_client(),
            ClientNetworkingEnum::Disconnected(WhoClosed::ThePeerDidntEvenConnect {
                failed_to_connect_event_already_polled,
            }) if !*failed_to_connect_event_already_polled => {
                *failed_to_connect_event_already_polled = true;
                Some(ClientEvent::FailedToConnect)
            }
            ClientNetworkingEnum::Disconnected(_who_closed) => None,
        }
    }

    /// Closes the connection with the server.
    pub fn disconnect(&mut self) -> DisconnectionHandle {
        match &mut self.0 {
            ClientNetworkingEnum::Connecting(connecting) => {
                let pending_messages = std::mem::take(&mut connecting.pending_sent_messages);
                for pending_message in pending_messages.into_iter() {
                    let _ = pending_message.result_sender.send(SendingResult::WeClosed);
                }
                self.0 = ClientNetworkingEnum::Disconnected(WhoClosed::Us);
                DisconnectionHandle::without_barrier()
            }
            ClientNetworkingEnum::Connected(connected) => {
                connected.connection.close(VarInt::from_u32(0), &[]);
                // Close properly.
                let endpoint = connected.endpoint.clone();
                let barrier = Arc::new(Barrier::new(2));
                let barrier_cloned = Arc::clone(&barrier);
                connected.async_runtime_handle.spawn(async move {
                    endpoint.wait_idle().await;
                    barrier_cloned.wait();
                });
                self.0 = ClientNetworkingEnum::Disconnected(WhoClosed::Us);
                DisconnectionHandle::with_barrier(barrier)
            }
            ClientNetworkingEnum::Disconnected(_who_closed) => {
                // TODO: Error? Is a double disconnection normal?
                DisconnectionHandle::without_barrier()
            }
        }
    }
}
