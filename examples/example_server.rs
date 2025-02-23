use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use rand::Rng;
use serde::{Deserialize, Serialize};
use warqueen::{
    ClientOnServerEvent, DisconnectionDetails, NetReceive, NetSend, ServerListenerNetworking,
};

#[derive(Deserialize, NetReceive)]
enum MessageClientToServer {
    String(String),
}

#[derive(Serialize, NetSend)]
enum MessageServerToClient {
    String(String),
}

fn main() {
    let desired_port = 21001;
    let address = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    // Full type is `ServerListenerNetworking<MessageServerToClient, MessageClientToServer>`.
    let server_listener = ServerListenerNetworking::new(address, desired_port);
    let actual_port = server_listener.server_port();
    println!("Opened on port {actual_port}");

    // List of connected clients, stored as tuples (client, id).
    // Full client type is `ClientOnServerNetworking<MessageServerToClient, MessageClientToServer>`.
    let mut clients = vec![];
    let mut next_client_id = 0;

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
        // Handling new clients connecting.
        while let Some(new_client) = server_listener.poll_client() {
            // Oh a new client connected!
            let address = new_client.client_address();
            let client_id = next_client_id;
            next_client_id += 1;
            println!("Connected to client at {address}, given id {client_id}");
            clients.push((new_client, client_id));
        }

        // Handling received messages from all clients.
        let mut disconnected_ids = vec![];
        for (client, client_id) in clients.iter() {
            while let Some(event) = client.poll_event_from_client() {
                match event {
                    ClientOnServerEvent::Message(message) => match message {
                        MessageClientToServer::String(content) => {
                            println!("Client {client_id} says \"{content}\"");
                        }
                    },
                    ClientOnServerEvent::Disconnected(details) => {
                        match details {
                            DisconnectionDetails::None => {
                                println!("Client {client_id} disconnected")
                            }
                            DisconnectionDetails::Timeout => {
                                println!("Client {client_id} timed out")
                            }
                        }
                        disconnected_ids.push(*client_id);
                    }
                }
            }
        }
        // Forget about clients that disconnected.
        clients.retain(|(_client, client_id)| !disconnected_ids.contains(client_id));

        // Periodically sending a message to a random client for the sake of the example.
        if last_sent_time.elapsed() > Duration::from_millis(2500) && !clients.is_empty() {
            last_sent_time = Instant::now();
            let random_client_index = rand::thread_rng().gen_range(0..clients.len());

            // Sending a message to a client.
            let (client, client_id) = &clients[random_client_index];
            println!("We say \"uwu\" to the client {client_id}");
            let message = MessageServerToClient::String("uwu".to_string());
            client.send_message_to_client(message);
        }

        if halt.load(Ordering::Relaxed) {
            // User hit Ctrl-C, we should close now.
            println!("Let's disconnect everyone");
            for (client, client_id) in clients {
                println!("Disconnecting client {client_id}");
                client.disconnect().wait_for_proper_disconnection();
            }
            return;
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}
