use serde::{de::DeserializeOwned, Serialize};

/// Message type that can be sent through the network.
///
/// A type marked by this trait can be the argument to the `S` type parameter of some
/// generic types like [`ServerListenerNetworking<S, R>`] or [`ClientNetworking<S, R>`],
/// which ends up as the type of the message argument to some related message sending methods.
///
/// If the default feature `derive` is enabled then it can be implemented by a derive macro.
pub trait NetSend: Serialize + Send + Sync + 'static {}

/// Message type that can be received from the network.
///
/// A type marked by this trait can be the argument to the `R` type parameter of some
/// generic types like [`ServerListenerNetworking<S, R>`] or [`ClientNetworking<S, R>`],
/// which ends up as the type returned by some related message receiving methods.
///
/// If the default feature `derive` is enabled then it can be implemented by a derive macro.
///
/// The bound to `DeserializeOwned` instead of `Deserialize<'de>`
/// is a nuance that can be ignored, just derive serde's `Deserialize` as usual.
pub trait NetReceive: DeserializeOwned + Send + Sync + 'static {}
