use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use warqueen::{NetReceive, NetSend, ServerNetworking};

#[derive(Serialize, Deserialize)]
enum MessageClientToServer {
	String(String),
}

#[derive(Serialize, Deserialize)]
enum MessageServerToClient {
	String(String),
}

impl NetSend for MessageClientToServer {}
impl NetReceive for MessageClientToServer {}
impl NetSend for MessageServerToClient {}
impl NetReceive for MessageServerToClient {}

fn main() {
	let port = 21001;
	let server = ServerNetworking::new(port);

	let mut clients = vec![];
	let mut last_send = Instant::now();
	let mut last_send_index = 0;

	loop {
		while let Some(client) = server.get_client() {
			println!("Connected to client at {}", client.client_address());
			clients.push(client);
		}

		if last_send.elapsed() > Duration::from_millis(2500) {
			last_send = Instant::now();
			if !clients.is_empty() {
				last_send_index = (last_send_index + 1) % clients.len();
				println!("We say \"uwu\" to a client");
				clients[last_send_index]
					.send_message_to_client(MessageServerToClient::String("uwu".to_string()));
			}
		}

		for client in clients.iter() {
			while let Some(message) = client.receive_message_from_client() {
				match message {
					MessageClientToServer::String(text) => println!("A client says \"{text}\""),
				}
			}
		}

		std::thread::sleep(Duration::from_millis(10));
	}
}
