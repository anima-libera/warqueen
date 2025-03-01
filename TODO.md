## Add

- Add a method (or some way) to `ClientNetworking` and `ClientOnServerNetworking` that returns a ping estimation.
- Add ability to constrain order between messages, like having message sends take a `Vec<OrderingConstraint>` and return a `MessageSendingId`, and `OrderingConstraint` is like an enum with variants like `After(MessageSendingId)` and stuff. That could require connections to keep a set of `MessageSendingId`s of the messages still being sent.
- Add a method to `ClientNetworking` and `ClientOnServerNetworking` that checks if all the messages that were being sent were actually sent (ie there is no messages still being sent at the moment).
- OR
  - Add a method to `ClientNetworking` and `ClientOnServerNetworking` that disconnects when all the messages that were sent prior to calling this method are finished being sent. *(Worst alternative)*
  - Make diconnecting accept a `Vec<OrderingConstraint>` to allow making sure that some messages arrive before the connection closes. *(Better alternative)*
- Return errors instead of panicking in cases where the problem is not a warqueen bug that should never happen.
- See that connections are kept alive even when no messages are being exchanged.
- Add a method to `DisconnectionHandle` that says in a non-blocking way if the disconnection has finished. Requires using an `AtomicBool` (maybe not instead of the `Barrier` but maybe in addition to it).
- Take a look at the certification and cryptography stuff offered by Quinn and allow to enable it.
- More simple examples.
- Tests.
- Add compression to the serialized messages, and make it configurable, maybe even by message.
- Add a way to get a network usage estimation, like the amount of data sent/received so far, or like that but by message.
- Make derive macros doc link to traits in Warqueen properly.
- Fix `ClonelessSending` doc link to example, make it point to the file in the GitHub repo.
- Rename `ClonelessSending::View::complete` to `access` or something.
- Add a method to `ServerListenerNetworking` to no longer accept new connections.
