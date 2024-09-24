
## 0.4.0

### Added

- Type `SendingStateHandle` that can be used to know if a specific message is still in the process of being sent or is done already, and if it was successfully sent or why it failed. Messages that fail being sent no longer cause a panic.
- Client-side event `ClientEvent::FailedToConnect` that lets the client know when the connecting to the server gives up and fails, instead of never being notified.

## 0.3.0

Commit `397dd6e` tagged [v0.3](https://github.com/anima-libera/warqueen/tree/v0.3).

### Added

- Type `ClonelessSending` that can be used in message types to allow senders to spare one clone of some data that is to be sent, only useful in a specific situation.
- A code example `both` that combines both the `client` and `server` code examples in one executable.
- A code example `cloneless` that builds on top of the `both` code example to demonstrate the use of `ClonelessSending`.
- This changelog ^^.

## 0.2.0

Commit `52f652e` tagged [v0.2](https://github.com/anima-libera/warqueen/tree/v0.2).

### Added

- Published v0.1.0 of sub-crate `warqueen_derive`, containing derive macros `NetSend` and `NetReceive` that implement the traits of the same name defined in `warqueen`.
- Re-exports of `warqueen_derive`'s two macros from `warqueen`.

## 0.1.0

Commit `6b365fc` tagged [v0.1](https://github.com/anima-libera/warqueen/tree/v0.1).

First released version.
