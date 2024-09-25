//! # Warqueen
//!
//! Hobby-scale small networking crate based on [Quinn](https://crates.io/crates/quinn),
//! message based, no async, no blocking.
//!
//! - The server and the client are intended to run in a loop.
//! - They can poll received messages and events when they want,
//! and send messages when they want.
//! - There is a message type for client-to-server messaging,
//! and a message type for server-to-client messaging, and that is all.
//! These two types can be enums to make up for that.
//! - Nya :3
//!
//! Both the client code and server code are supposed to have vaguely the following structure:
//! ```ignore
//! let mut networking_stuff = ...;
//! loop {
//!     while let Some(event) = poll_event(&mut networking_stuff) {
//!         handle_event(event);
//!     }
//!     handle_other_networking_stuff(&mut networking_stuff);
//!     // ...
//! }
//! ```
//!
//! Go see the examples, they are very simple, probably the best guide for Warqueen!
//!
//! Also the [README](https://github.com/anima-libera/warqueen/blob/main/README.md)
//! that is displayed on the [crates.io page](https://crates.io/crates/warqueen)
//! has somewhat of a usage guide.
//!
//! Things to keep in mind:
//! - Do not use, lacks plenty of features.
//! - Beware the [`DisconnectionHandle`]s that are better waited for on the main thread
//! (if you have multiple threads and disconnect from a thread other than the main thread).
//! - Good luck out there!

mod async_runtime;
mod client;
mod cloneless;
mod disconnection;
mod net_traits;
mod receiving;
mod sending;
mod server;

// Opt-out derive macros.
#[cfg(feature = "derive")]
pub use warqueen_derive::{NetReceive, NetSend};

pub use client::{ClientEvent, ClientNetworking};
pub use cloneless::ClonelessSending;
pub use disconnection::DisconnectionHandle;
pub use net_traits::{NetReceive, NetSend};
pub use sending::{SendingResult, SendingState, SendingStateHandle};
pub use server::{ClientOnServerEvent, ClientOnServerNetworking, ServerListenerNetworking};
