use std::{
    marker::PhantomData,
    net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs},
    num::NonZeroUsize,
    sync::{mpsc::Receiver, Arc, Barrier},
};

use quinn::{
    crypto::rustls::QuicServerConfig, default_runtime, Connection, ConnectionError, Endpoint,
    EndpointConfig, ReadError, ReadToEndError, VarInt,
};
use rustls::pki_types::PrivatePkcs8KeyDer;
use tokio::{runtime::Handle, sync::oneshot};

use crate::{
    async_runtime::async_runtime,
    disconnection::DisconnectionHandle,
    net_traits::{NetReceive, NetSend},
    receiving::receive_message_raw,
    sending::{send_message, SendingResult, SendingStateHandle},
};

// TODO: Do something about this >w<.
// How is it used by Quinn? Is it useful? Should it be configurable by the user?
pub(crate) const SERVER_NAME: &str = "jaaj";

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
    /// Opens a `ServerListenerNetworking`.
    ///
    /// Providing a desired port will make the socket use that port if possible,
    /// although it might not be availabe and port that follow will be tried
    /// until one is available.
    ///
    /// Always use [`ServerListenerNetworking::server_port`]
    /// to know for sure what port is actually used.
    pub fn new(
        desired_port: Option<u16>,
        address: Option<IpAddr>,
        thread_count: Option<NonZeroUsize>,
    ) -> ServerListenerNetworking<S, R> {
        rustls::crypto::ring::default_provider()
            .install_default()
            .unwrap();

        let async_runtime_handle = async_runtime(thread_count);
        let async_runtime_handle_cloned = async_runtime_handle.clone();

        let (client_sender, client_receiver) = std::sync::mpsc::channel();

        let server_config = {
            let cert = rcgen::generate_simple_self_signed(vec![SERVER_NAME.into()]).unwrap();
            let key = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()).into();
            let certs = vec![cert.cert.into()];
            let server_crypto = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap();
            quinn::ServerConfig::with_crypto(Arc::new(
                QuicServerConfig::try_from(server_crypto).unwrap(),
            ))
        };

        let socket = if let Some(desired_port) = desired_port {
            // We have a desired port to try in priority,
            // but if we just pass it to `bind` and it fails then that would be it.
            //
            // `std::net::UdpSocket::bind` can try multiple socket addresses until one works.
            // If the desired port is not available then we just try the port that follows,
            // and the next, etc., until we find an available port that is hopefully not too far.
            //
            // The doc of this very function says that this is what might happen.
            #[derive(Clone)]
            struct SocketAddrsToTry {
                ip_addr: IpAddr,
                next_port: u16,
            }
            impl Iterator for SocketAddrsToTry {
                type Item = SocketAddr;
                fn next(&mut self) -> Option<SocketAddr> {
                    let port = self.next_port;
                    self.next_port = self.next_port.wrapping_add(1);
                    Some(SocketAddr::new(self.ip_addr, port))
                }
            }
            impl ToSocketAddrs for SocketAddrsToTry {
                type Iter = SocketAddrsToTry;
                fn to_socket_addrs(&self) -> std::io::Result<SocketAddrsToTry> {
                    Ok(self.clone())
                }
            }
            let addresses_to_try = SocketAddrsToTry {
                ip_addr: address.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
                next_port: desired_port,
            };
            std::net::UdpSocket::bind(addresses_to_try).unwrap()
        } else {
            // There is no desired port specified,
            // we use the wildcard port and be done with it.
            const PORT_UNSPECIFIED: u16 = 0;
            let socket_address = SocketAddr::new(
                address.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
                PORT_UNSPECIFIED,
            );
            std::net::UdpSocket::bind(socket_address).unwrap()
        };

        let actual_server_address = socket.local_addr().unwrap();

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
    /// let server = ServerListenerNetworking::new(Some(port), None, None);
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
    Disconnected(ClientOnServerDisconnectionDetails),
}

/// Details about a disconnection event [`ClientOnServerEvent::Disconnected`].
pub enum ClientOnServerDisconnectionDetails {
    None,
    /// The client timed out (failed to react in time to stuff).
    Timeout,
}

fn connection_error_to_client_on_server_event<R: NetReceive>(
    error: ConnectionError,
) -> Option<ClientOnServerEvent<R>> {
    match error {
        ConnectionError::ApplicationClosed(_thingy) => {
            // TODO: Deserialize the reason from `_thingy` and put it in the event.
            Some(ClientOnServerEvent::Disconnected(
                ClientOnServerDisconnectionDetails::None,
            ))
        }
        ConnectionError::ConnectionClosed(_thingy) => {
            // TODO: Deserialize the reason from `_thingy` and put it in the event.
            Some(ClientOnServerEvent::Disconnected(
                ClientOnServerDisconnectionDetails::None,
            ))
        }
        ConnectionError::LocallyClosed => {
            // Our own side have closed the connection, let's just wrap up as expected.
            None
        }
        ConnectionError::TimedOut => Some(ClientOnServerEvent::Disconnected(
            ClientOnServerDisconnectionDetails::Timeout,
        )),
        error => {
            // TODO: Handle more errors to pass as events to the user.
            panic!("{error}");
        }
    }
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
                            match receive_message_raw(&mut stream).await {
                                Ok(message_raw) => {
                                    // Received all the message successfully!
                                    let message: R =
                                        rmp_serde::decode::from_slice(&message_raw).unwrap();
                                    let event = ClientOnServerEvent::Message(message);
                                    let _ = receiving_sender_cloned.send(event);
                                }
                                Err(ReadToEndError::Read(ReadError::ConnectionLost(error))) => {
                                    // Oh we lost the connection in the middle of receiving
                                    // the message.
                                    let event = connection_error_to_client_on_server_event(error);
                                    if let Some(event) = event {
                                        let _ = receiving_sender_cloned.send(event);
                                    }
                                }
                                Err(error) => {
                                    // TODO: Handle more errors to pass as events to the user.
                                    panic!("{error}");
                                }
                            }
                        });
                    }
                    Err(error) => {
                        // We just lost the connection.
                        let event = connection_error_to_client_on_server_event(error);
                        if let Some(event) = event {
                            let _ = receiving_sender.send(event);
                        }
                        return;
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
    /// # let server = ServerListenerNetworking::new(Some(port), None, None);
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
        SendingStateHandle::from_result_receiver(result_receiver)
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
    /// # let server = ServerListenerNetworking::new(Some(port), None, None);
    /// let client = server.poll_client().unwrap();
    ///
    /// loop {
    ///     while let Some(event) = client.poll_event_from_client() {
    ///         match event {
    ///             ClientOnServerEvent::Message(message) => match message {
    ///                 MessageClientToServer::Hello => { /* ... */ },
    ///                 // Handle the different possible message variants...
    ///             },
    ///             ClientOnServerEvent::Disconnected(details) => {
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
