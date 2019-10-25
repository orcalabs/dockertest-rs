//! Contains `WaitFor` trait used to determine when a PendingContainer has started
//! and all the default implementations of it.

use crate::container::Container;
use failure::Error;
use futures::future::Future;

mod message;
mod nowait;
mod status;

pub use message::{MessageSource, MessageWait};
pub use nowait::NoWait;
pub use status::{ExitedWait, RunningWait};

/// Trait to wait for a container to be ready for service.
pub trait WaitFor {
    /// Method implementation should return a future that resolves once the condition
    /// described by the implementing structure is fulfilled. Once this successfully resolves,
    /// the container is marked as ready.
    fn wait_for_ready(
        &self,
        container: Container,
    ) -> Box<dyn Future<Item = Container, Error = Error>>;
}