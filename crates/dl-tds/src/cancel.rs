//! Cancelling a running request from another thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A flag shared between a [`Client`](crate::Client) and the thread that may stop it.
///
/// Set it with [`Client::set_cancel`](crate::Client::set_cancel); [`CancelToken::cancel`]
/// from any thread makes the running request send ATTENTION and return
/// [`Error::Cancelled`](crate::Error::Cancelled). Clones share the flag.
///
/// ```
/// let token = dl_tds::CancelToken::new();
/// let for_stop = token.clone();
/// for_stop.cancel();
/// assert!(token.is_cancelled());
/// ```
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that is not cancelled.
    pub fn new() -> CancelToken {
        CancelToken::default()
    }

    /// Asks every client holding this token to stop its request.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// `true` once [`CancelToken::cancel`] was called.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
