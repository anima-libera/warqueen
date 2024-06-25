# Warqueen - Hobby-scale networking crate

Message based, no async, no blocking. Uses the awesome [Quinn](https://crates.io/crates/quinn) networking crate.

As the client, just connect, send messages, and poll received messages whenever, in a non-blocking way, without async. As the server, same stuff, and poll new client connections whenever. Can do that in a few loops, very simple API, easy to understand and use.

Message types are constrainted to only one possible type for sending and one possible type for receiving, swapping these type roles for the server and clients. An enum for both is natural.

## Usage

Let's start with a client.

First define the two message types for the communication with the server. The type that the client can send (to the server) and the type that the client can receive (from the server).

```rust
#[derive(Serialize, Deserialize)]
enum MessageClientToServer {
	String(String),
}
impl NetSend for MessageClientToServer {}

#[derive(Serialize, Deserialize)]
enum MessageServerToClient {
	String(String),
}
impl NetReceive for MessageServerToClient {}
```

Then create the `ClientNetworking` that connects to a server, and use it in a loop.

```rust
// Full type is `ClientNetworking<MessageClientToServer, MessageServerToClient>`.
let mut client = ClientNetworking::new(server_address);

loop {
	// Handling received messages from the server, as well as connection events.
	while let Some(event) = client.poll_event_from_server() {
		match event {
			ClientEvent::Message(message) => match message {
				MessageServerToClient::String(content) => println!("The server says \"{content}\""),
			},
			ClientEvent::Connected => println!("Connected"),
			ClientEvent::Disconnected => println!("Disconnected"),
		}
	}

	// Sending a message to the server.
	let message = MessageClientToServer::String("hiii :3".to_string());
	client.send_message_to_server(message);*

	sleep(Duration::from_millis(10));
}
```

Note that the connection is not established immediately, it may take some time, and when it finally happens there would be a `ClientEvent::Connected` event polled to notify the client. If the connection is never established, then such event won't ever be polled; a timeout error may be relevant in such case.

When it is time to go, the connection can be closed properly as follows.

```rust
client.disconnect().wait_for_proper_disconnection();
```

Note that if the call to `ClientNetworking::disconnect` is not made in the main thread, then the `DisconnectionHandle` that it returns should be passed to the main thread (for example via a channel) and only then should `DisconnectionHandle::wait_for_proper_disconnection` be called. See the documentation for more.

A server is very similar, it should use the same message types as the client, except with the traits `NetSend` and `NetReceive` implemented the other way around (because the server sends whan the client receives, and vice versa). If both the server and the client use the same type definitions then it is even better: both types can implement both traits.

Then create the `ServerListenerNetworking` that listens for new clients that want to connect, and use it in a loop.

```rust
// Full type is `ServerListenerNetworking<MessageServerToClient, MessageClientToServer>`.
let server_listener = ServerListenerNetworking::new(desired_port);
let mut clients = vec![];

loop {
	// Handling new clients connections.
	while let Some(client) = server_listener.poll_client() {
		// Client type is `ClientOnServerNetworking<MessageServerToClient, MessageClientToServer>`.
		println!("Connected to a client at {}", client.client_address());
		clients.push(client);
	}

	// Handling clients...

	sleep(Duration::from_millis(10));
}
```

Clients polled from `ServerListenerNetworking::poll_client` are of type `ClientOnServerNetworking` and are basically the server-side equivalent of `ClientNetworking`. For each connected `ClientNetworking` in a client program, there is a corresponding `ClientOnServerNetworking` in a server program.

```rust
// Handling received messages as well as connection events from all clients.
for (index, client) in clients.iter().enumerate() {
	while let Some(event) = client.poll_event_from_client() {
		match event {
			ClientOnServerEvent::Message(message) => match message {
				MessageClientToServer::String(content) => {
					println!("Client {index} says \"{content}\"");
				},
			},
			ClientOnServerEvent::Disconnected => {
				println!("Client {index} disconnected");
			},
		}
	}
}

// Sending a message to a client.
let message = MessageServerToClient::String("nyaa~".to_string());
clients[some_index_whatever].send_message_to_client(message);
```

Note that the server-side equivalent of the `ClientEvent::Connected` event is the fact that `ServerListenerNetworking::poll_client` returns a connection to a client, there is no need for a `ClientOnServerEvent::Connected` because a `ClientOnServerNetworking` is already connected to the client when created.

Disconnecting is the same as for clients, it also comes with a `DisconnectionHandle` (for each disconnection) to take special care to carry to the main thread if the diconnection is not made on the main thread.

```rust
clients[some_index_whatever].disconnect().wait_for_proper_disconnection();
```

## Scale

Hobby-scale! Just a small wrapper around [Quinn](https://crates.io/crates/quinn) (best Rust netwroking crate for game networking?) to get an API that I like. It may be suited for game networking, hopefully! At least I am using it in my game.

See the [TODO list](TODO.md) for potential future features. Also don't hesitate to contribute!

Note that any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as below, without any additional terms or conditions.

## License

Copyright 2024 Jeanne DEMOUSSEL.

This project is licensed under either of

- [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0) ([`LICENSE-APACHE`](LICENSE-APACHE))
- [MIT license](https://opensource.org/licenses/MIT) ([`LICENSE-MIT`](LICENSE-MIT))

at your option.
