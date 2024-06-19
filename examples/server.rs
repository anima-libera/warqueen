use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use warqueen::{NetReceive, NetSend, ServerListenerNetworking};

#[derive(Serialize, Deserialize)]
enum MessageClientToServer {
	String(String),
}

#[derive(Serialize, Deserialize)]
enum MessageServerToClient {
	String(String),
}

impl NetReceive for MessageClientToServer {}
impl NetSend for MessageServerToClient {}

fn main() {
	let desired_port = 21001;
	let server_listener = ServerListenerNetworking::new(desired_port);
	let actual_port = server_listener.server_port();
	println!("Opened on port {actual_port}");

	// List of connected clients, stored as tuples (client, id).
	let mut clients = vec![];
	let mut next_client_id = 0;

	let mut last_sent_time = Instant::now();
	let mut last_sent_index = 0;

	loop {
		// Handling new clinets connecting.
		while let Some(new_client) = server_listener.poll_client() {
			// Oh a new client connected!
			let address = new_client.client_address();
			let client_id = next_client_id;
			next_client_id += 1;
			println!("Connected to client at {address}, given id {client_id}");
			clients.push((new_client, client_id));
		}

		// Handling received messages from all clients.
		for (client, client_id) in clients.iter() {
			while let Some(message) = client.receive_message_from_client() {
				match message {
					MessageClientToServer::String(content) => {
						println!("Client {client_id} says \"{content}\"");
					},
				}
			}
		}

		// Periodically sending a message to a client for the sake of the example.
		if last_sent_time.elapsed() > Duration::from_millis(2500) && !clients.is_empty() {
			last_sent_time = Instant::now();
			last_sent_index = (last_sent_index + 1) % clients.len();

			// Sending a message to a client.
			let (client, client_id) = &clients[last_sent_index];
			println!("We say \"uwu\" to the client {client_id}");
			let message = MessageServerToClient::String("uwu".to_string());
			client.send_message_to_client(message);
		}

		std::thread::sleep(Duration::from_millis(10));
	}
}
