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
impl NetReceive for MessageClientToServer {}
impl NetSend for MessageServerToClient {}
impl NetReceive for MessageServerToClient {}

fn main() {
	let server_address = "127.0.0.1:21001".parse().unwrap();
	let mut client = ClientNetworking::new(server_address);

	let mut last_send = Instant::now();
	loop {
		while let Some(message) = client.receive_message_from_server() {
			match message {
				MessageServerToClient::String(string) => println!("The server says \"{string}\""),
			}
		}

		if last_send.elapsed() > Duration::from_millis(1500) {
			last_send = Instant::now();
			println!("We say \"jaaj\" to the server");
			client.send_message_to_server(MessageServerToClient::String("jaaj".to_string()));
		}

		std::thread::sleep(Duration::from_millis(10));
	}
}
