use std::sync::{Arc, Barrier};

/// A thingy that allows to make sure that a disconnection has the time to happen properly
/// before process termination.
///
/// If the disconnection is not given the time to finish properly, then the other end
/// will not be properly notified that the connection is closed and will have to
/// timeout to notice, which is so rude!
///
/// When the main thread terminates, all the other threads are killed, so only the main thread
/// should have the responsability to wait for the disconnection to properly end
/// (because other threads could just be killed by the main thread's termination).
/// For this reason, a `DisconnectionHandle` should be passed to the main thread
/// and only then call [`DisconnectionHandle::wait_for_proper_disconnection`].
///
/// If you do not care about this or know that waiting in whatever thread is fine,
/// then call [`DisconnectionHandle::wait_for_proper_disconnection_while_not_on_the_main_thread`],
/// not unsafe but has a long name.
///
/// Should not be dropped without the waiting being done.
/// If the `forbid_handle_drop` feature is enabled then unwaited handle drop panics.
#[must_use = "Not making sure that the diconnection happens before process exit is bad"]
pub struct DisconnectionHandle {
    barrier: Option<Arc<Barrier>>,
    waited_for: bool,
}

impl DisconnectionHandle {
    pub(crate) fn with_barrier(barrier: Arc<Barrier>) -> DisconnectionHandle {
        DisconnectionHandle {
            barrier: Some(barrier),
            waited_for: false,
        }
    }

    pub(crate) fn without_barrier() -> DisconnectionHandle {
        DisconnectionHandle {
            barrier: None,
            waited_for: false,
        }
    }

    /// Wait for the diconnection that returned this handle to properly happen.
    ///
    /// It should be fast, but should be done. When diconnecting and then terminating
    /// the process, the process termination can happen too fast and cut the networking
    /// thread half way through its process of properly disconnecting.
    ///
    /// For every disconnection that you do not wait properly for, a kitten feel sad.
    /// Do not sadden kitties, please wait for proper disconnections 🥺.
    pub fn wait_for_proper_disconnection(mut self) {
        Self::check_that_we_are_on_the_main_thread();
        self.actually_wait_for_proper_disconnection();
    }

    /// If you are reaaallly sure that you can wait just fine on this thread
    /// that may not be the main thread, then call this method.
    ///
    /// Same as [`DisconnectionHandle::wait_for_proper_disconnection`] but
    /// doesn't warns if called from a thread that is not the main thread.
    pub fn wait_for_proper_disconnection_while_not_on_the_main_thread(mut self) {
        self.actually_wait_for_proper_disconnection();
    }

    fn actually_wait_for_proper_disconnection(&mut self) {
        if let Some(barrier) = &self.barrier {
            barrier.wait();
        }
        self.waited_for = true;

        // TODO: What happens when the other thread panics before the wait call?
        // Does it just blocks here forever?
        // Maybe we should find a way to block with a timeout here instead,
        // like what about a 1 second timeout, or a user-chosen timeout?
    }

    fn check_that_we_are_on_the_main_thread() {
        if let Some(is_main_thread_answer) = is_main_thread::is_main_thread() {
            if is_main_thread_answer {
                // Nice, we are in the main thread, this is where we should wait
                // (because it is the main thread that kills all the others when it terminates).
            } else {
                println!(
                    "Warning: `ClientDisconnectionHandle::wait_for_proper_disconnection` \
					should be called in the main thread, see documentation as to why"
                )
            }
        }
    }
}

impl Drop for DisconnectionHandle {
    fn drop(&mut self) {
        if !self.waited_for {
            if cfg!(feature = "forbid_handle_drop") {
                if !std::thread::panicking() {
                    panic!(
                        "`ClientDisconnectionHandle` dropped \
						instead of being intentionally waited for"
                    );
                }
            } else {
                println!(
                    "Warning: `ClientDisconnectionHandle` dropped \
					instead of being intentionally waited for"
                );
                Self::check_that_we_are_on_the_main_thread();
                self.actually_wait_for_proper_disconnection();
            }
        }
    }
}
