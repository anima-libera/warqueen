use std::{
	marker::PhantomData,
	net::{IpAddr, Ipv4Addr, SocketAddr},
	sync::{mpsc::Receiver, Arc},
	time::Duration,
};

use quinn::{
	crypto::rustls::{QuicClientConfig, QuicServerConfig},
	default_runtime, ClientConfig, Connection, ConnectionError, Endpoint, EndpointConfig,
	RecvStream,
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

/// Message type that is used for sending have to impl this trait.
pub trait NetSend: Serialize + Send + 'static {}

/// Message type that is used for receiving have to impl this trait.
pub trait NetReceive: DeserializeOwned + Send + 'static {}

/// A piece of server networking that establishes connections to new clients
/// and provide these new clients (in the form of `ClientOnServerNetworking`s)
/// when asked for. Should be asked for in a loop.
pub struct ServerNetworking<S: NetSend, R: NetReceive> {
	// TODO: Remove? Seems to be unused.
	_async_runtime_handle: Handle,
	local_port: u16,
	client_receiver: Receiver<ClientOnServerNetworking<S, R>>,
}

impl<S: NetSend, R: NetReceive> ServerNetworking<S, R> {
	pub fn new(port: u16) -> ServerNetworking<S, R> {
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
		let server_address = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
		let socket = std::net::UdpSocket::bind(server_address).unwrap();
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

					let client =
						ClientOnServerNetworking::new(async_runtime_handle_cloned.clone(), connection);

					client_sender.send(client).unwrap();
				}
			});
		});

		ServerNetworking {
			_async_runtime_handle: async_runtime_handle,
			local_port: actual_server_address.port(),
			client_receiver,
		}
	}

	pub fn server_port(&self) -> u16 {
		self.local_port
	}

	pub fn get_client(&self) -> Option<ClientOnServerNetworking<S, R>> {
		self.client_receiver.try_recv().ok()
	}
}

pub struct ClientOnServerNetworking<S: NetSend, R: NetReceive> {
	async_runtime_handle: Handle,
	connection: Connection,
	receiving_receiver: Receiver<R>,
	_phantom: PhantomData<S>,
}

impl<S: NetSend, R: NetReceive> ClientOnServerNetworking<S, R> {
	fn new(async_runtime_handle: Handle, connection: Connection) -> ClientOnServerNetworking<S, R> {
		let (receiving_sender, receiving_receiver) = std::sync::mpsc::channel();
		let connection_cloned = connection.clone();
		tokio::spawn(async move {
			loop {
				let mut stream = match connection_cloned.accept_uni().await {
					Ok(stream) => stream,
					Err(ConnectionError::ApplicationClosed(thingy)) => {
						let aids = String::from_utf8(thingy.reason.to_vec()).unwrap();
						println!("connection closed due to {aids}");
						return;
					},
					Err(error) => panic!("{error}"),
				};
				let receiving_sender_cloned = receiving_sender.clone();
				tokio::spawn(async move {
					let message_raw = receive_message_raw(&mut stream).await;
					let message: R = rmp_serde::decode::from_slice(&message_raw).unwrap();
					receiving_sender_cloned.send(message).unwrap();
				});
			}
		});

		ClientOnServerNetworking {
			async_runtime_handle,
			connection,
			receiving_receiver,
			_phantom: PhantomData,
		}
	}

	pub fn client_address(&self) -> SocketAddr {
		self.connection.remote_address()
	}

	// Note: Could take `impl NetSend` instead of `S`, but then it won't
	// look like the client-side API.
	pub fn send_message_to_client(&self, message: S) {
		let connection = self.connection.clone();
		self.async_runtime_handle.spawn(async move {
			send_message(&connection, message).await;
		});
	}

	pub fn receive_message_from_client(&self) -> Option<R> {
		self.receiving_receiver.try_recv().ok()
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
	receiving_receiver: Receiver<R>,
	_phantom: PhantomData<S>,
}

enum ClientNetworkingEnum<S: NetSend, R: NetReceive> {
	/// The connection is still in the process of being established.
	/// When connected, we transition to the `Connected` variant.
	Connecting(ClientNetworkingConnecting<S, R>),
	Connected(ClientNetworkingConnected<S, R>),
}

pub struct ClientNetworking<S: NetSend, R: NetReceive>(ClientNetworkingEnum<S, R>);

impl<S: NetSend, R: NetReceive> ClientNetworkingConnecting<S, R> {
	fn new(server_address: SocketAddr) -> ClientNetworkingConnecting<S, R> {
		rustls::crypto::ring::default_provider().install_default().unwrap();

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
				ClientNetworkingConnected::new(async_runtime_handle_cloned, connection);

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
	fn new(async_runtime_handle: Handle, connection: Connection) -> ClientNetworkingConnected<S, R> {
		let (receiving_sender, receiving_receiver) = std::sync::mpsc::channel();

		let connection_cloned = connection.clone();
		tokio::spawn(async move {
			loop {
				let mut stream = match connection_cloned.accept_uni().await {
					Ok(stream) => stream,
					Err(ConnectionError::ApplicationClosed(thingy)) => {
						let aids = String::from_utf8(thingy.reason.to_vec()).unwrap();
						println!("connection closed due to {aids}");
						return;
					},
					Err(error) => panic!("{error}"),
				};
				let receiving_sender_cloned = receiving_sender.clone();
				tokio::spawn(async move {
					let message_raw = receive_message_raw(&mut stream).await;
					let message: R = rmp_serde::decode::from_slice(&message_raw).unwrap();
					receiving_sender_cloned.send(message).unwrap();
				});
			}
		});

		ClientNetworkingConnected {
			async_runtime_handle,
			connection,
			receiving_receiver,
			_phantom: PhantomData,
		}
	}

	pub fn send_message_to_server(&self, message: S) {
		let connection = self.connection.clone();
		self.async_runtime_handle.spawn(async move {
			send_message(&connection, message).await;
		});
	}

	pub fn receive_message_from_server(&self) -> Option<R> {
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
		}
	}

	pub fn receive_message_from_server(&mut self) -> Option<R> {
		self.connect_if_possible();
		match &self.0 {
			ClientNetworkingEnum::Connecting(_connecting) => None,
			ClientNetworkingEnum::Connected(connected) => connected.receive_message_from_server(),
		}
	}
}
