use std::{
	sync::{
		atomic::{AtomicBool, Ordering},
		Arc,
	},
	time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use warqueen::{ClientEvent, ClientNetworking, NetReceive, NetSend};

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

	// Make sure hitting Ctrl-C lets us close the connection properly.
	let halt = Arc::new(AtomicBool::new(false));
	let halt_cloned = Arc::clone(&halt);
	ctrlc::set_handler(move || {
		println!();
		halt_cloned.store(true, Ordering::Relaxed);
	})
	.unwrap();

	loop {
		// Handling received messages from the server.
		while let Some(event) = client.poll_event_from_client() {
			match event {
				ClientEvent::Message(message) => match message {
					MessageServerToClient::String(content) => println!("The server says \"{content}\""),
				},
				ClientEvent::Disconnected => {
					println!("Server disconnected");
					return;
				},
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

		if halt.load(Ordering::Relaxed) {
			// User hit Ctrl-C, we should close now.
			println!("Let's disconnect");
			client.disconnect();
			// Make sure the disconnection had time to be sent to the other side properly.
			std::thread::sleep(Duration::from_millis(100));
			break;
		}

		std::thread::sleep(Duration::from_millis(10));
	}
}
