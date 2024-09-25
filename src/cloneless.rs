use std::sync::Arc;

use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};

/// Allows a sender to clonelessly send the content `T` as part of a message
/// in the specific situation where:
/// - a `&T` can be accessed from a `&C`, and
/// - the `C` has a chance to be in an `Arc<C>`, and
/// - we want to avoid cloning `T` if it can be avoided (for example if T is big).
///
/// In short, in the situation described above, a message type could
/// contain a `ClonelessSending<T, C>` instead of a `T`.
/// A `ClonelessSending<T, C>` can be just like a `T` (when it has the `Owned` variant)
/// and the receiver of such message will only see `Owned` variants of `ClonelessSending`s
/// in the messages it receives.
///
/// A `ClonelessSending<T, C>` can be different from just a `T` though,
/// it can be an `Arc<C>` accompanied by a function that allows to access a `&T` in a `&C`.
/// This allows the sender to avoid cloning the `T` in the `Arc<C>` to put it in a message to send.
///
/// Even when a sender avoids cloning by providing a `View` variant, the receiver will
/// receive the accessed `T` in an `Owned` variant.
/// This is so thanks to [`serde::Serialize`] being implemented for `ClonelessSending<T, C>` in
/// a way that just serializes the `T` it allows to access, and [`serde::Deserialize`] just
/// deserializing a `T` into a `ClonelessSending::Owned(T)`.
///
/// *See the [`cloneless` example](../examples/cloneless.rs) for a proper usage guide.*
pub enum ClonelessSending<T: Serialize + DeserializeOwned, C> {
    Owned(T),
    View { arc: Arc<C>, complete: fn(&C) -> &T },
}

impl<T: Serialize + DeserializeOwned, C> ClonelessSending<T, C> {
    /// Consumes and returns the contained owned `T`.
    ///
    /// Panics if it was not owned.
    /// The received `ClonelessSending`s are always owned, this method is intended
    /// to be used by the receiver to extract the received owned `T`s.
    pub fn into_owned(self) -> T {
        if let ClonelessSending::Owned(content) = self {
            content
        } else {
            panic!("Completable::into_owned called on a non-owned variant");
        }
    }
}

impl<T: Serialize + DeserializeOwned, C> Serialize for ClonelessSending<T, C> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ClonelessSending::Owned(content) => serializer.serialize_some(content),
            ClonelessSending::View { arc, complete } => {
                let content = complete(arc.as_ref());
                serializer.serialize_some(content)
            }
        }
    }
}

impl<'de, T: Serialize + DeserializeOwned, C> Deserialize<'de> for ClonelessSending<T, C> {
    fn deserialize<D>(deserializer: D) -> Result<ClonelessSending<T, C>, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(ClonelessSending::Owned)
    }
}
