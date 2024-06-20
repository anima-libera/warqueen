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
//! - Nyaa :3
//!
//! Both the client code and server code are supposed to have the following structure:
//! ```ignore
//! let mut networking_stuff = ...;
//! loop {
//!     handle_networking_stuff(&mut networking_stuff);
//!     // ...
//! }
//! ```
//!
//! Go see the examples, they are very simple, probably the best guide for Warqueen!
//!
//! Things to keep in mind:
//! - Do not use, lacks plenty of features.
//! - Beware the [`DisconnectionHandle`]s that are better waited for on the main thread.
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
	RecvStream, VarInt,
};
use rustls::pki_types::PrivatePkcs8KeyDer;
use serde::{de::DeserializeOwned, Serialize};
use tokio::runtime::Handle;

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

async fn send_message(connection: &Connection, message: impl Serialize) {
	let mut stream = connection.open_uni().await.unwrap();
	let message_raw = rmp_serde::encode::to_vec(&message).unwrap();
	stream.write_all(&message_raw).await.unwrap();
	stream.finish().unwrap();
	stream.stopped().await.unwrap();
}

async fn receive_message_raw(stream: &mut RecvStream) -> Vec<u8> {
	const ONE_GIGABYTE_IN_BYTES: usize = 1073741824;
	stream.read_to_end(ONE_GIGABYTE_IN_BYTES).await.unwrap()
}

/// A thingy that allows to make sure that a disconnection has the time to happen properly.
///
/// When the main thread terminates, all the other threads are killed, so only the main thread
/// should have the responsability to wait for the disconnection to properly end
/// (because other threads could just be killed by the main thread's termination).
/// For this reason, a `DisconnectionHandle` should be passed to the main thread
/// and only then [`DisconnectionHandle::wait_for_proper_disconnection`].
///
/// Should not be dropped without the waiting being done.
#[must_use = "Not making sure that the diconnection happens before process exit is bad"]
pub struct DisconnectionHandle {
	barrier: Option<Arc<Barrier>>,
	waited_for: bool,
}

impl DisconnectionHandle {
	fn with_barrier(barrier: Arc<Barrier>) -> DisconnectionHandle {
		DisconnectionHandle { barrier: Some(barrier), waited_for: false }
	}

	fn without_barrier() -> DisconnectionHandle {
		DisconnectionHandle { barrier: None, waited_for: false }
	}

	/// Wait for the diconnection that returned this handle to properly happen.
	///
	/// It should be fast, but should be done. When diconnecting and then terminating
	/// the process, the process termination can happen too fast and cut the networking
	/// thread half way through its process of properly disconnecting.
	///
	/// For every disconnection that you do not wait properly for, a kitten feel sad.
	/// Do not make kittens sad, be a good person and wait for proper disconnections.
	pub fn wait_for_proper_disconnection(mut self) {
		Self::check_that_we_are_on_the_main_thread();
		self.actually_wait_for_proper_disconnection();
	}

	/// If you are reaaallly sure that you can wait just fine on this thread
	/// that is not the main thread, then you do you.
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
					should be called in the main thread, see documentation as to why."
				)
			}
		}
	}
}

impl Drop for DisconnectionHandle {
	fn drop(&mut self) {
		if !self.waited_for {
			println!(
				"Warning: `ClientDisconnectionHandle` dropped \
				instead of being intentionally waited for."
			);
			self.actually_wait_for_proper_disconnection();
		}
	}
}

/// Message type that is used for sending have to impl this trait.
pub trait NetSend: Serialize + Send + 'static {}

/// Message type that is used for receiving have to impl this trait.
pub trait NetReceive: DeserializeOwned + Send + 'static {}

/// A piece of server networking that establishes connections to new clients
/// and provides these new clients (in the form of `ClientOnServerNetworking`s)
/// when asked for. Should be asked for in a loop, see examples.
///
/// `S` and `R` are the message types that can be send and received respectively,
/// see the sending and receiving methods of `ClientOnServerNetworking<S, R>`.
pub struct ServerListenerNetworking<S: NetSend, R: NetReceive> {
	// TODO: Remove? Seems to be unused.
	_async_runtime_handle: Handle,
	local_port: u16,
	client_receiver: Receiver<ClientOnServerNetworking<S, R>>,
}

impl<S: NetSend, R: NetReceive> ServerListenerNetworking<S, R> {
	pub fn new(desired_port: u16) -> ServerListenerNetworking<S, R> {
		rustls::crypto::ring::default_provider().install_default().unwrap();

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

/// A connection to a client, from a server's perspective.
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
					},
					Err(ConnectionError::ApplicationClosed(_thingy)) => {
						// TODO: Deserialize the reason from `_thingy` and put it in the event.
						let event = ClientOnServerEvent::Disconnected;
						receiving_sender.send(event).unwrap();
						return;
					},
					Err(ConnectionError::ConnectionClosed(_thingy)) => {
						// TODO: Deserialize the reason from `_thingy` and put it in the event.
						let event = ClientOnServerEvent::Disconnected;
						receiving_sender.send(event).unwrap();
						return;
					},
					Err(ConnectionError::LocallyClosed) => {
						// Our own side have closed the connection, let's just wrap up as expected.
						return;
					},
					Err(error) => {
						// TODO: Handle more errors to pass as events to the user.
						panic!("{error}");
					},
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
	pub fn send_message_to_client(&self, message: S) {
		let connection = self.connection.clone();
		self.async_runtime_handle.spawn(async move {
			send_message(&connection, message).await;
		});
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
	/// Used only once.
	connected_client_receiver: Receiver<ClientNetworkingConnected<S, R>>,
	/// A message sent while the connection is still being established
	/// is stored here and will be sent once the connection is established.
	// Note: This is the reason why we have the `S` type on all the client-side types
	// (and it was put on the server-side types as well for symetry >w<).
	pending_sent_messages: Vec<S>,
}

struct ClientNetworkingConnected<S: NetSend, R: NetReceive> {
	async_runtime_handle: Handle,
	connection: Connection,
	endpoint: Endpoint,
	receiving_receiver: Receiver<ClientEvent<R>>,
	_phantom: PhantomData<S>,
}

/// Returned by [`ClientNetworking::poll_event_from_server`].
///
/// Describes an event that happened regarding the connection to a server.
pub enum ClientEvent<R: NetReceive> {
	/// The server sent us a message.
	Message(R),
	/// We got disconnected from the server.
	Disconnected,
}

enum ClientNetworkingEnum<S: NetSend, R: NetReceive> {
	/// The connection is still in the process of being established.
	/// When connected, we transition to the `Connected` variant.
	Connecting(ClientNetworkingConnecting<S, R>),
	Connected(ClientNetworkingConnected<S, R>),
	Disconnected,
}

/// A connection to a server, from a client's perspective.
///
/// `S` and `R` are the message types that can be send and received respectively.
pub struct ClientNetworking<S: NetSend, R: NetReceive>(ClientNetworkingEnum<S, R>);

impl<S: NetSend, R: NetReceive> ClientNetworkingConnecting<S, R> {
	fn new(server_address: SocketAddr) -> ClientNetworkingConnecting<S, R> {
		rustls::crypto::ring::default_provider().install_default().unwrap();

		let async_runtime_handle = async_runtime();

		let (connected_client_sender, connected_client_receiver) = std::sync::mpsc::channel();

		let async_runtime_handle_cloned = async_runtime_handle.clone();

		async_runtime_handle.spawn(async move {
			let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();

			endpoint.set_default_client_config(ClientConfig::new(Arc::new(
				QuicClientConfig::try_from(
					rustls::ClientConfig::builder()
						.dangerous()
						.with_custom_certificate_verifier(cerificate_verifier::EveryoneIsValid::new())
						.with_no_client_auth(),
				)
				.unwrap(),
			)));

			let connection = endpoint.connect(server_address, SERVER_NAME).unwrap().await.unwrap();

			let connected_client =
				ClientNetworkingConnected::new(async_runtime_handle_cloned, connection, endpoint);

			connected_client_sender.send(connected_client).unwrap();
		});

		ClientNetworkingConnecting { connected_client_receiver, pending_sent_messages: vec![] }
	}

	fn connected(&mut self) -> Option<ClientNetworkingConnected<S, R>> {
		self.connected_client_receiver.try_recv().ok().inspect(|connected_client| {
			let pending_sent_messages = std::mem::take(&mut self.pending_sent_messages);
			for message in pending_sent_messages.into_iter() {
				connected_client.send_message_to_server(message);
			}
		})
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
					},
					Err(ConnectionError::ApplicationClosed(_thingy)) => {
						// TODO: Deserialize the reason from `_thingy` and put it in the event.
						let event = ClientEvent::Disconnected;
						receiving_sender.send(event).unwrap();
						return;
					},
					Err(ConnectionError::ConnectionClosed(_thingy)) => {
						// TODO: Deserialize the reason from `_thingy` and put it in the event.
						let event = ClientEvent::Disconnected;
						receiving_sender.send(event).unwrap();
						return;
					},
					Err(ConnectionError::LocallyClosed) => {
						// Our own side have closed the connection, let's just wrap up as expected.
						return;
					},
					Err(error) => {
						// TODO: Handle more errors to pass as events to the user.
						panic!("{error}");
					},
				}
			}
		});

		ClientNetworkingConnected {
			async_runtime_handle,
			connection,
			endpoint,
			receiving_receiver,
			_phantom: PhantomData,
		}
	}

	fn send_message_to_server(&self, message: S) {
		let connection = self.connection.clone();
		self.async_runtime_handle.spawn(async move {
			send_message(&connection, message).await;
		});
	}

	fn poll_event_from_client(&self) -> Option<ClientEvent<R>> {
		self.receiving_receiver.try_recv().ok()
	}
}

impl<S: NetSend, R: NetReceive> ClientNetworking<S, R> {
	pub fn new(server_address: SocketAddr) -> ClientNetworking<S, R> {
		ClientNetworking(ClientNetworkingEnum::Connecting(
			ClientNetworkingConnecting::new(server_address),
		))
	}

	/// Transition to the `Connected` variant if we finally established the connection.
	fn connect_if_possible(&mut self) {
		if let ClientNetworkingEnum::Connecting(connecting) = &mut self.0 {
			if let Some(connected) = connecting.connected() {
				self.0 = ClientNetworkingEnum::Connected(connected);
			}
		}
	}

	/// Just sends the given message to the server.
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
	pub fn send_message_to_server(&mut self, message: S) {
		self.connect_if_possible();
		match &mut self.0 {
			ClientNetworkingEnum::Connecting(connecting) => {
				connecting.pending_sent_messages.push(message);
			},
			ClientNetworkingEnum::Connected(connected) => {
				let connection = connected.connection.clone();
				connected.async_runtime_handle.spawn(async move {
					send_message(&connection, message).await;
				});
			},
			ClientNetworkingEnum::Disconnected => {
				// TODO: Error maybe?
			},
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
	///             ClientEvent::Disconnected => {
	///                 // Handle the server disconnection...
	///             },
	///         }
	///     }
	/// }
	/// #
	/// # client.send_message_to_server(MessageClientToServer::Hello);
	/// ```
	pub fn poll_event_from_server(&mut self) -> Option<ClientEvent<R>> {
		self.connect_if_possible();
		match &self.0 {
			ClientNetworkingEnum::Connecting(_connecting) => None,
			ClientNetworkingEnum::Connected(connected) => connected.poll_event_from_client(),
			ClientNetworkingEnum::Disconnected => None,
		}
	}

	/// Closes the connection with the server.
	pub fn disconnect(&mut self) -> DisconnectionHandle {
		match &self.0 {
			ClientNetworkingEnum::Connecting(_connecting) => {
				// TODO: What do we do here?
				self.0 = ClientNetworkingEnum::Disconnected;
				DisconnectionHandle::without_barrier()
			},
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
				self.0 = ClientNetworkingEnum::Disconnected;
				DisconnectionHandle::with_barrier(barrier)
			},
			ClientNetworkingEnum::Disconnected => {
				// TODO: Error? Is a double disconnection normal?
				DisconnectionHandle::without_barrier()
			},
		}
	}
}
