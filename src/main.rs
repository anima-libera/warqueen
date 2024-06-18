use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use warqueen::{self, ClientNetworking, NetReceive, NetSend, ServerNetworking};

fn main() {
	if std::env::args().any(|arg| arg == "client") {
		client();
	} else if std::env::args().any(|arg| arg == "server") {
		server();
	} else {
		panic!("select what is to be run");
	}
}

#[derive(Serialize, Deserialize)]
enum MessageClientToServer {
	String(String),
	End,
}

#[derive(Serialize, Deserialize)]
enum MessageServerToClient {
	String(String),
	End,
}

impl NetSend for MessageClientToServer {}
impl NetReceive for MessageClientToServer {}
impl NetSend for MessageServerToClient {}
impl NetReceive for MessageServerToClient {}

fn server() {
	let port = 21001;
	let server = ServerNetworking::new(port);
	let mut clients = vec![];
	let mut last_send = Instant::now();
	let mut last_send_index = 0;

	loop {
		while let Some(client) = server.get_client() {
			clients.push(client);
		}

		if last_send.elapsed() > Duration::from_millis(500) {
			last_send = Instant::now();
			if !clients.is_empty() {
				last_send_index = (last_send_index + 1) % clients.len();
				clients[last_send_index].send_message(MessageServerToClient::String("uwu".to_string()));
			}
		}

		for client in clients.iter() {
			while let Some(message) = client.receive_message() {
				match message {
					MessageClientToServer::String(text) => println!("client says \"{text}\""),
					MessageClientToServer::End => println!("client ends"),
				}
			}
		}

		std::thread::sleep(Duration::from_millis(10));
	}
}

fn client() {
	let server_address = "127.0.0.1:21001".parse().unwrap();
	let mut client = ClientNetworking::new(server_address);
	let mut last_send = Instant::now();

	loop {
		while let Some(message) = client.receive_message() {
			match message {
				MessageServerToClient::String(string) => println!("{string}"),
				MessageServerToClient::End => println!("end"),
			}
		}

		if last_send.elapsed() > Duration::from_millis(1500) {
			last_send = Instant::now();
			client.send_message(MessageServerToClient::String("jaaj".to_string()));
		}

		std::thread::sleep(Duration::from_millis(10));
	}
}
