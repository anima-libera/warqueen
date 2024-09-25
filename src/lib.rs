//! # Warqueen
//!
//! Hobby-scale small networking crate based on [Quinn](https://crates.io/crates/quinn),
//! message based, no async, no blocking.
//!
//! - The server and the client are intended to run in a loop.
//! - They can poll received messages and events when they want,
//! and send messages when they want.
//! - There is a message type for client-to-server messaging,
//! and a message type for server-to-client messaging, and that is all.
//! These two types can be enums to make up for that.
//! - Nya :3
//!
//! Both the client code and server code are supposed to have vaguely the following structure:
//! ```ignore
//! let mut networking_stuff = ...;
//! loop {
//!     while let Some(event) = poll_event(&mut networking_stuff) {
//!         handle_event(event);
//!     }
//!     handle_other_networking_stuff(&mut networking_stuff);
//!     // ...
//! }
//! ```
//!
//! Go see the examples, they are very simple, probably the best guide for Warqueen!
//!
//! Also the [README](https://github.com/anima-libera/warqueen/blob/main/README.md)
//! that is displayed on the [crates.io page](https://crates.io/crates/warqueen)
//! has somewhat of a usage guide.
//!
//! Things to keep in mind:
//! - Do not use, lacks plenty of features.
//! - Beware the [`DisconnectionHandle`]s that are better waited for on the main thread
//! (if you have multiple threads and disconnect from a thread other than the main thread).
//! - Good luck out there!

use std::{
    marker::PhantomData,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{mpsc::Receiver, Arc, Barrier},
    time::Duration,
};

use quinn::{
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
    default_runtime, ClientConfig, Connection, ConnectionError, Endpoint, EndpointConfig,
    RecvStream, StoppedError, VarInt, WriteError,
};
use rustls::pki_types::PrivatePkcs8KeyDer;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};
use tokio::{runtime::Handle, sync::oneshot};

// Opt-out derive macros.
#[cfg(feature = "derive")]
pub use warqueen_derive::{NetReceive, NetSend};

// TODO: Do something about this >w<.
// How is it used by Quinn? Is it useful? Should it be configurable by the user?
const SERVER_NAME: &str = "jaaj";

fn async_runtime() -> Handle {
    let async_runtime = tokio::runtime::Builder::new_current_thread()
        .thread_name("Tokio Runtime Thread")
        .enable_time()
        .enable_io()
        .build()
        .unwrap();
    let async_runtime_handle = async_runtime.handle().clone();
    std::thread::Builder::new()
        .name("Tokio Runtime Thread".to_string())
        .spawn(move || {
            async_runtime.block_on(async {
                loop {
                    tokio::time::sleep(Duration::from_millis(1)).await
                }
            })
        })
        .unwrap();

    async_runtime_handle
}

/// When a message is sent, it is sent asynchronously over a period.
/// This handle allows to get to know if the message was really properly sent in the end
/// or if something went wrong and what, the information is not available immediately.
pub struct SendingStateHandle {
    result_receiver: oneshot::Receiver<SendingResult>,
}

impl SendingStateHandle {
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

    fn from_result(result: Result<(), SendingError>) -> SendingResult {
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
enum SendingError {
    OpenUni(ConnectionError),
    WriteAll(WriteError),
    Stopped(StoppedError),
}

async fn send_message(
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

async fn receive_message_raw(stream: &mut RecvStream) -> Vec<u8> {
    const ONE_GIGABYTE_IN_BYTES: usize = 1073741824;
    stream.read_to_end(ONE_GIGABYTE_IN_BYTES).await.unwrap()
}

/// Allows a sender to clonelessly send the content `T` as part of a message
/// in the specific situation where:
/// - a `&T` can be accessed from a `&C`, and
/// - the `C` has a chance to be in an `Arc<C>`, and
/// - we want to avoid cloning `T` if it can be avoided (for example if T is big).
///
/// In short, in the situation described above, a message type could
/// contain a `ClonelessSending<T, C>` instead of a `T`.
/// A `ClonelessSending<T, C>` can be just like a `T` (when it has the `Owned` variant)
/// and the receiver of such message will only see `Owned` variants of `ClonelessSending`s
/// in the messages it receives.
///
/// A `ClonelessSending<T, C>` can be different from just a `T` though,
/// it can be an `Arc<C>` accompanied by a function that allows to access a `&T` in a `&C`.
/// This allows the sender to avoid cloning the `T` in the `Arc<C>` to put it in a message to send.
///
/// Even when a sender avoids cloning by providing a `View` variant, the receiver will
/// receive the accessed `T` in an `Owned` variant.
/// This is so thanks to [`serde::Serialize`] being implemented for `ClonelessSending<T, C>` in
/// a way that just serializes the `T` it allows to access, and [`serde::Deserialize`] just
/// deserializing a `T` into a `ClonelessSending::Owned(T)`.
///
/// *See the [`cloneless` example](../examples/cloneless.rs) for a proper usage guide.*
pub enum ClonelessSending<T: Serialize + DeserializeOwned, C> {
    Owned(T),
    View { arc: Arc<C>, complete: fn(&C) -> &T },
}

impl<T: Serialize + DeserializeOwned, C> ClonelessSending<T, C> {
    /// Consumes and returns the contained owned `T`.
    ///
    /// Panics if it was not owned.
    /// The received `ClonelessSending`s are always owned, this method is intended
    /// to be used by the receiver to extract the received owned `T`s.
    pub fn into_owned(self) -> T {
        if let ClonelessSending::Owned(content) = self {
            content
        } else {
            panic!("Completable::into_owned called on a non-owned variant");
        }
    }
}

impl<T: Serialize + DeserializeOwned, C> Serialize for ClonelessSending<T, C> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ClonelessSending::Owned(content) => serializer.serialize_some(content),
            ClonelessSending::View { arc, complete } => {
                let content = complete(arc.as_ref());
                serializer.serialize_some(content)
            }
        }
    }
}

impl<'de, T: Serialize + DeserializeOwned, C> Deserialize<'de> for ClonelessSending<T, C> {
    fn deserialize<D>(deserializer: D) -> Result<ClonelessSending<T, C>, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(ClonelessSending::Owned)
    }
}

/// A thingy that allows to make sure that a disconnection has the time to happen properly
/// before process termination.
///
/// If the disconnection is not given the time to finish properly, then the other end
/// will not be properly notified that the connection is closed and will have to
/// timeout to notice, which is so rude!
///
/// When the main thread terminates, all the other threads are killed, so only the main thread
/// should have the responsability to wait for the disconnection to properly end
/// (because other threads could just be killed by the main thread's termination).
/// For this reason, a `DisconnectionHandle` should be passed to the main thread
/// and only then call [`DisconnectionHandle::wait_for_proper_disconnection`].
///
/// If you do not care about this or know that waiting in whatever thread is fine,
/// then call [`DisconnectionHandle::wait_for_proper_disconnection_while_not_on_the_main_thread`],
/// not unsafe but has a long name.
///
/// Should not be dropped without the waiting being done.
/// If the `forbid_handle_drop` feature is enabled then unwaited handle drop panics.
#[must_use = "Not making sure that the diconnection happens before process exit is bad"]
pub struct DisconnectionHandle {
    barrier: Option<Arc<Barrier>>,
    waited_for: bool,
}

impl DisconnectionHandle {
    fn with_barrier(barrier: Arc<Barrier>) -> DisconnectionHandle {
        DisconnectionHandle {
            barrier: Some(barrier),
            waited_for: false,
        }
    }

    fn without_barrier() -> DisconnectionHandle {
        DisconnectionHandle {
            barrier: None,
            waited_for: false,
        }
    }

    /// Wait for the diconnection that returned this handle to properly happen.
    ///
    /// It should be fast, but should be done. When diconnecting and then terminating
    /// the process, the process termination can happen too fast and cut the networking
    /// thread half way through its process of properly disconnecting.
    ///
    /// For every disconnection that you do not wait properly for, a kitten feel sad.
    /// Do not sadden kitties, please wait for proper disconnections 🥺.
    pub fn wait_for_proper_disconnection(mut self) {
        Self::check_that_we_are_on_the_main_thread();
        self.actually_wait_for_proper_disconnection();
    }

    /// If you are reaaallly sure that you can wait just fine on this thread
    /// that may not be the main thread, then call this method.
    ///
    /// Same as [`DisconnectionHandle::wait_for_proper_disconnection`] but
    /// doesn't warns if called from a thread that is not the main thread.
    pub fn wait_for_proper_disconnection_while_not_on_the_main_thread(mut self) {
        self.actually_wait_for_proper_disconnection();
    }

    fn actually_wait_for_proper_disconnection(&mut self) {
        if let Some(barrier) = &self.barrier {
            barrier.wait();
        }
        self.waited_for = true;

        // TODO: What happens when the other thread panics before the wait call?
        // Does it just blocks here forever?
        // Maybe we should find a way to block with a timeout here instead,
        // like what about a 1 second timeout, or a user-chosen timeout?
    }

    fn check_that_we_are_on_the_main_thread() {
        if let Some(is_main_thread_answer) = is_main_thread::is_main_thread() {
            if is_main_thread_answer {
                // Nice, we are in the main thread, this is where we should wait
                // (because it is the main thread that kills all the others when it terminates).
            } else {
                println!(
                    "Warning: `ClientDisconnectionHandle::wait_for_proper_disconnection` \
					should be called in the main thread, see documentation as to why"
                )
            }
        }
    }
}

impl Drop for DisconnectionHandle {
    fn drop(&mut self) {
        if !self.waited_for {
            if cfg!(feature = "forbid_handle_drop") {
                if !std::thread::panicking() {
                    panic!(
                        "`ClientDisconnectionHandle` dropped \
						instead of being intentionally waited for"
                    );
                }
            } else {
                println!(
                    "Warning: `ClientDisconnectionHandle` dropped \
					instead of being intentionally waited for"
                );
                Self::check_that_we_are_on_the_main_thread();
                self.actually_wait_for_proper_disconnection();
            }
        }
    }
}

/// Message type that can be sent through the network.
///
/// A type marked by this trait can be the argument to the `S` type parameter of some
/// generic types like [`ServerListenerNetworking<S, R>`] or [`ClientNetworking<S, R>`],
/// which ends up as the type of the message argument to some related message sending methods.
///
/// If the default feature `derive` is enabled then it can be implemented by a derive macro.
pub trait NetSend: Serialize + Send + Sync + 'static {}

/// Message type that can be received from the network.
///
/// A type marked by this trait can be the argument to the `R` type parameter of some
/// generic types like [`ServerListenerNetworking<S, R>`] or [`ClientNetworking<S, R>`],
/// which ends up as the type returned by some related message receiving methods.
///
/// If the default feature `derive` is enabled then it can be implemented by a derive macro.
///
/// The bound to `DeserializeOwned` instead of `Deserialize<'de>`
/// is a nuance that can be ignored, just derive serde's `Deserialize` as usual.
pub trait NetReceive: DeserializeOwned + Send + Sync + 'static {}

/// A piece of server networking that establishes connections to new clients
/// and provides these new clients (in the form of `ClientOnServerNetworking`s)
/// when asked for.
///
/// Should be asked for in a loop, see examples and [`ServerListenerNetworking::poll_client`].
///
/// `S` and `R` are the message types that can be send and received respectively,
/// used by the sending and receiving methods of [`ClientOnServerNetworking<S, R>`].
pub struct ServerListenerNetworking<S: NetSend, R: NetReceive> {
    // TODO: Remove? Seems to be unused.
    _async_runtime_handle: Handle,
    local_port: u16,
    client_receiver: Receiver<ClientOnServerNetworking<S, R>>,
}

impl<S: NetSend, R: NetReceive> ServerListenerNetworking<S, R> {
    /// Opens a `ServerListenerNetworking` on the given desired port hopefully.
    ///
    /// The port actually used may be different from the desired port,
    /// see [`ServerListenerNetworking::server_port`].
    pub fn new(desired_port: u16) -> ServerListenerNetworking<S, R> {
        rustls::crypto::ring::default_provider()
            .install_default()
            .unwrap();

        let async_runtime_handle = async_runtime();

        let (client_sender, client_receiver) = std::sync::mpsc::channel();

        let cert = rcgen::generate_simple_self_signed(vec![SERVER_NAME.into()]).unwrap();
        let key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        let certs = vec![cert.cert.into()];
        let key = key.into();
        let server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
        let server_config = quinn::ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(server_crypto).unwrap(),
        ));

        let desired_server_address =
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), desired_port);
        let socket = std::net::UdpSocket::bind(desired_server_address).unwrap();
        let actual_server_address = socket.local_addr().unwrap();

        let async_runtime_handle_cloned = async_runtime_handle.clone();

        async_runtime_handle.spawn(async move {
            // Basically `Endpoint::server` except we made the socket ourselves
            // and outside the async block so that we could get its actual port immediately.
            let endpoint = Endpoint::new(
                EndpointConfig::default(),
                Some(server_config),
                socket,
                default_runtime().unwrap(),
            )
            .unwrap();

            tokio::spawn(async move {
                loop {
                    let connection = endpoint.accept().await.unwrap().await.unwrap();

                    let client = ClientOnServerNetworking::new(
                        async_runtime_handle_cloned.clone(),
                        connection,
                        endpoint.clone(),
                    );

                    client_sender.send(client).unwrap();
                }
            });
        });

        ServerListenerNetworking {
            _async_runtime_handle: async_runtime_handle,
            local_port: actual_server_address.port(),
            client_receiver,
        }
    }

    /// The port the server listens on.
    /// Could happen to be a bit different from the desired port given at creation.
    pub fn server_port(&self) -> u16 {
        self.local_port
    }

    /// If new clients are connected to the server, then returns one of them.
    ///
    /// A new client connection can be seen as an event that should be polled in a loop.
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
    /// # impl NetSend for MessageServerToClient {}
    /// #
    /// # #[derive(Serialize, Deserialize)]
    /// # enum MessageClientToServer {
    /// #     Hello,
    /// #     // ...
    /// # }
    /// # impl NetReceive for MessageClientToServer {}
    /// #
    /// # let port = 21001;
    /// let server = ServerListenerNetworking::new(port);
    ///
    /// loop {
    ///     while let Some(new_client) = server.poll_client() {
    ///         // Add the client to a list or something...
    ///     }
    ///     // ...
    /// }
    /// #
    /// # let client = server.poll_client().unwrap();
    /// # let _: ClientOnServerEvent<MessageClientToServer> =
    /// #     client.poll_event_from_client().unwrap();
    /// # client.send_message_to_client(MessageServerToClient::Hello);
    /// ```
    pub fn poll_client(&self) -> Option<ClientOnServerNetworking<S, R>> {
        self.client_receiver.try_recv().ok()
    }
}

/// A connection to a client, from a server's point of view.
///
/// Returned by [`ServerListenerNetworking::poll_client`].
///
/// `S` and `R` are the message types that can be send and received respectively.
pub struct ClientOnServerNetworking<S: NetSend, R: NetReceive> {
    async_runtime_handle: Handle,
    connection: Connection,
    endpoint: Endpoint,
    receiving_receiver: Receiver<ClientOnServerEvent<R>>,
    _phantom: PhantomData<S>,
}

/// Returned by [`ClientOnServerNetworking::poll_event_from_client`].
///
/// Describes an event that happened regarding the connection to a client.
pub enum ClientOnServerEvent<R: NetReceive> {
    /// The client sent us a message.
    Message(R),
    /// We got disconnected from the client.
    Disconnected,
}

impl<S: NetSend, R: NetReceive> ClientOnServerNetworking<S, R> {
    fn new(
        async_runtime_handle: Handle,
        connection: Connection,
        endpoint: Endpoint,
    ) -> ClientOnServerNetworking<S, R> {
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
                            let event = ClientOnServerEvent::Message(message);
                            receiving_sender_cloned.send(event).unwrap();
                        });
                    }
                    Err(ConnectionError::ApplicationClosed(_thingy)) => {
                        // TODO: Deserialize the reason from `_thingy` and put it in the event.
                        let event = ClientOnServerEvent::Disconnected;
                        receiving_sender.send(event).unwrap();
                        return;
                    }
                    Err(ConnectionError::ConnectionClosed(_thingy)) => {
                        // TODO: Deserialize the reason from `_thingy` and put it in the event.
                        let event = ClientOnServerEvent::Disconnected;
                        receiving_sender.send(event).unwrap();
                        return;
                    }
                    Err(ConnectionError::LocallyClosed) => {
                        // Our own side have closed the connection, let's just wrap up as expected.
                        return;
                    }
                    Err(error) => {
                        // TODO: Handle more errors to pass as events to the user.
                        panic!("{error}");
                    }
                };
            }
        });

        ClientOnServerNetworking {
            async_runtime_handle,
            connection,
            endpoint,
            receiving_receiver,
            _phantom: PhantomData,
        }
    }

    /// The address of the client, at the other end of this connection.
    pub fn client_address(&self) -> SocketAddr {
        self.connection.remote_address()
    }

    /// Just sends the given message to that client.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use serde::{Serialize, Deserialize};
    /// # use warqueen::*;
    /// #
    /// #[derive(Serialize, Deserialize)]
    /// enum MessageServerToClient {
    ///     Hello,
    ///     // ...
    /// }
    /// impl NetSend for MessageServerToClient {}
    ///
    /// # #[derive(Serialize, Deserialize)]
    /// # enum MessageClientToServer {
    /// #     Hello,
    /// #     // ...
    /// # }
    /// # impl NetReceive for MessageClientToServer {}
    /// #
    /// # let port = 21001;
    /// # let server = ServerListenerNetworking::new(port);
    /// let client = server.poll_client().unwrap();
    ///
    /// client.send_message_to_client(MessageServerToClient::Hello);
    /// #
    /// # let _: ClientOnServerEvent<MessageClientToServer> =
    /// #     client.poll_event_from_client().unwrap();
    /// ```
    // Note: Could take `impl NetSend` instead of `S`, but then it won't
    // look like the client-side API.
    pub fn send_message_to_client(&self, message: S) -> SendingStateHandle {
        let connection = self.connection.clone();
        let (result_sender, result_receiver) = oneshot::channel();
        self.async_runtime_handle.spawn(async move {
            let result = send_message(&connection, &message).await;
            let _ = result_sender.send(SendingResult::from_result(result));
        });
        SendingStateHandle { result_receiver }
    }

    /// If that client has sent any new messages, returns one of them.
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
    /// # impl NetSend for MessageServerToClient {}
    /// #
    /// #[derive(Serialize, Deserialize)]
    /// enum MessageClientToServer {
    ///     Hello,
    ///     // ...
    /// }
    /// impl NetReceive for MessageClientToServer {}
    ///
    /// # let port = 21001;
    /// # let server = ServerListenerNetworking::new(port);
    /// let client = server.poll_client().unwrap();
    ///
    /// loop {
    ///     while let Some(event) = client.poll_event_from_client() {
    ///         match event {
    ///             ClientOnServerEvent::Message(message) => match message {
    ///                 MessageClientToServer::Hello => { /* ... */ },
    ///                 // Handle the different possible message variants...
    ///             },
    ///             ClientOnServerEvent::Disconnected => {
    ///                 // Handle the client disconnection...
    ///             },
    ///         }
    ///     }
    /// }
    /// #
    /// # client.send_message_to_client(MessageServerToClient::Hello);
    /// ```
    pub fn poll_event_from_client(&self) -> Option<ClientOnServerEvent<R>> {
        self.receiving_receiver.try_recv().ok()
    }

    pub fn disconnect(&self) -> DisconnectionHandle {
        self.connection.close(VarInt::from_u32(0), &[]);
        // Close properly.
        let endpoint = self.endpoint.clone();
        let barrier = Arc::new(Barrier::new(2));
        let barrier_cloned = Arc::clone(&barrier);
        self.async_runtime_handle.spawn(async move {
            endpoint.wait_idle().await;
            barrier_cloned.wait();
        });
        DisconnectionHandle::with_barrier(barrier)
    }
}

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
    Disconnected,
    /// We could not even establish a connection (in a reasonable amount of time).
    FailedToConnect,
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
            let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();

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
                        let event = ClientEvent::Disconnected;
                        receiving_sender.send(event).unwrap();
                        return;
                    }
                    Err(ConnectionError::ConnectionClosed(_thingy)) => {
                        // TODO: Deserialize the reason from `_thingy` and put it in the event.
                        let event = ClientEvent::Disconnected;
                        receiving_sender.send(event).unwrap();
                        return;
                    }
                    Err(ConnectionError::LocallyClosed) => {
                        // Our own side have closed the connection, let's just wrap up as expected.
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
        SendingStateHandle { result_receiver }
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
                SendingStateHandle { result_receiver }
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
                SendingStateHandle { result_receiver }
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
    ///             ClientEvent::Disconnected => {
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
