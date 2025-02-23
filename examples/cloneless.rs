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
    ClientEvent, ClientNetworking, ClientOnServerEvent, ClonelessSending, DisconnectionDetails,
    NetReceive, NetSend, ServerListenerNetworking,
};

// See `example_client.rs` and `example_server.rs` for more comments.
// This examples focuces on a specific feature that allows the sender to avoid cloning
// a part of a potentially big part of a message in a specific situation.

/// Lots of data that we wouldn't want to clone when it can be avoided.
#[derive(Serialize, Deserialize)]
struct LotsOfData {
    /// Big!
    wow_big_vec: Vec<u8>,
}

/// We want to be able to send (and receive) `SomeStruct::data` in messages.
/// This is destined to be put in an Arc.
/// If the message sending API wants to own the messages (instead of just a reference to it)
/// it is because sendig a message takes time in an other thread and we want to guarantee
/// that the message data is still alive during its serialization.
/// That can be done by moving the data to the sending API, or giving it a cloned version of
/// the data; but what if the data is in an Arc (not moving) and expensive to clone?
/// The `Arc<SomeStruct>` could be `Arc::cloned` and passsd to the sending API to guarantee
/// that `SomeStruct::data` is alive long enough to be serialize at its own pace.
/// We could send a message containing an `Arc<SomeStruct>` after all,
/// but here there is an catch: we only want to send a part of `SomeStruct`, not all of it.
/// This is the problem that is solved by the use of [`warqueen::Late`]
/// demonstrated in this example.
struct SomeStruct {
    /// This is some stuff that is alongside `data` in the `Arc<SomeStruct>`,
    /// but we do not want to have to send this through the network >_<.
    _dont_wanna_send_that: Vec<u8>,
    /// This is not in an Arc by itself, its parent is in an Arc.
    data: LotsOfData,
}

#[derive(Serialize, Deserialize, NetSend, NetReceive)]
enum MessageClientToServer {
    String(String),
    /// Here is the message variant that contains the data from `SomeStruct::data`,
    /// and the `SomeStruct` is in an Arc.
    /// The [`warqueen::Late`] wrapper allows the sender to
    /// avoid cloning `SomeStruct::data` (which is nice)
    /// while still being serialized into something that the receiver will own.
    LotsOfData(ClonelessSending<LotsOfData, SomeStruct>),
}

#[derive(Serialize, Deserialize, NetSend, NetReceive)]
enum MessageServerToClient {
    String(String),
}

fn client() {
    let struct_in_arc = Arc::new(SomeStruct {
        _dont_wanna_send_that: vec![8],
        data: LotsOfData {
            wow_big_vec: std::iter::repeat(69).take(2000).collect(),
        },
    });
    // Here, note that `struct_in_arc.data` has a lot of data
    // (more than one thousand!! who can even count that high!?).
    // Also, since we prentend that we are a complex program that passes this data around
    // in threads and all, it is already in an Arc.

    let server_address = "127.0.0.1:21001".parse().unwrap();
    let mut client = ClientNetworking::new(server_address);

    let mut last_sent_time = Instant::now();

    let halt = Arc::new(AtomicBool::new(false));
    let halt_cloned = Arc::clone(&halt);
    ctrlc::set_handler(move || {
        println!();
        halt_cloned.store(true, Ordering::Relaxed);
    })
    .unwrap();

    loop {
        while let Some(event) = client.poll_event_from_server() {
            match event {
                ClientEvent::Connected => {
                    println!("Connected");
                }
                ClientEvent::Message(message) => match message {
                    MessageServerToClient::String(content) => {
                        println!("The server says \"{content}\"")
                    }
                },
                ClientEvent::Disconnected => {
                    println!("Server disconnected, let's terminate");
                    return;
                }
                ClientEvent::FailedToConnect => {
                    println!("Failed to connect, let's terminate");
                    return;
                }
            }
        }

        if last_sent_time.elapsed() > Duration::from_millis(1500) {
            last_sent_time = Instant::now();

            println!("We send `struct_in_arc.data` to the server");
            let message = MessageClientToServer::LotsOfData(ClonelessSending::View {
                arc: Arc::clone(&struct_in_arc),
                // Here we explain to the serializer how to access the data we want to send
                // in the `Arc`ed data that we share with it.
                complete: |some_struct: &SomeStruct| &some_struct.data,
            });
            client.send_message_to_server(message);
        }

        if halt.load(Ordering::Relaxed) {
            println!("Let's disconnect");
            client.disconnect().wait_for_proper_disconnection();
            break;
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

fn server() {
    let desired_port = 21001;
    let address = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let server_listener = ServerListenerNetworking::new(address, desired_port);
    let actual_port = server_listener.server_port();
    println!("Opened on port {actual_port}");

    let mut clients = vec![];
    let mut next_client_id = 0;

    let mut last_sent_time = Instant::now();

    let halt = Arc::new(AtomicBool::new(false));
    let halt_cloned = Arc::clone(&halt);
    ctrlc::set_handler(move || {
        println!();
        halt_cloned.store(true, Ordering::Relaxed);
    })
    .unwrap();

    loop {
        while let Some(new_client) = server_listener.poll_client() {
            let address = new_client.client_address();
            let client_id = next_client_id;
            next_client_id += 1;
            println!("Connected to client at {address}, given id {client_id}");
            clients.push((new_client, client_id));
        }

        let mut disconnected_ids = vec![];
        for (client, client_id) in clients.iter() {
            while let Some(event) = client.poll_event_from_client() {
                match event {
                    ClientOnServerEvent::Message(message) => match message {
                        MessageClientToServer::String(content) => {
                            println!("Client {client_id} says \"{content}\"");
                        }
                        MessageClientToServer::LotsOfData(content) => {
                            // Just like that, the receiver is not even really inconvenienced.
                            // It owns the content of the `ClonelessSending`,
                            // it receives the `Owned` variants
                            // even if the sender used the `View` variant to send the message.
                            let content = content.into_owned();
                            let size = content.wow_big_vec.len();
                            println!(
                                "Client {client_id} sent {size} bytes of data, that is a lot!"
                            );
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
        clients.retain(|(_client, client_id)| !disconnected_ids.contains(client_id));

        if last_sent_time.elapsed() > Duration::from_millis(2500) && !clients.is_empty() {
            last_sent_time = Instant::now();
            let random_client_index = rand::thread_rng().gen_range(0..clients.len());

            let (client, client_id) = &clients[random_client_index];
            println!("We say \"uwu\" to the client {client_id}");
            let message = MessageServerToClient::String("uwu".to_string());
            client.send_message_to_client(message);
        }

        if halt.load(Ordering::Relaxed) {
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

fn main() {
    if std::env::args().any(|arg| arg.contains("client")) {
        client();
    } else if std::env::args().any(|arg| arg.contains("server")) {
        server();
    } else {
        panic!("Neither `client` nor `server` specified in command line");
    }
}
