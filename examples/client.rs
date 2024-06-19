use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use warqueen::{ClientNetworking, NetReceive, NetSend};

#[derive(Serialize, Deserialize)]
enum MessageClientToServer {
	String(String),
}

#[derive(Serialize, Deserialize)]
enum MessageServerToClient {
	String(String),
}

impl NetSend for MessageClientToServer {}
impl NetReceive for MessageServerToClient {}

fn main() {
	let server_address = "127.0.0.1:21001".parse().unwrap();
	let mut client = ClientNetworking::new(server_address);

	let mut last_sent_time = Instant::now();

	loop {
		// Handling received messages from the server.
		while let Some(message) = client.receive_message_from_server() {
			match message {
				MessageServerToClient::String(content) => println!("The server says \"{content}\""),
			}
		}

		// Periodically sending a message to a client for the sake of the example.
		if last_sent_time.elapsed() > Duration::from_millis(1500) {
			last_sent_time = Instant::now();

			// Sending a message to the server.
			println!("We say \"jaaj\" to the server");
			let message = MessageClientToServer::String("jaaj".to_string());
			client.send_message_to_server(message);
		}

		std::thread::sleep(Duration::from_millis(10));
	}
}
