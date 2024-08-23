//! Contains `WaitFor` trait used to determine when a PendingContainer has started
//! and all the default implementations of it.

use crate::container::{OperationalContainer, PendingContainer};
use crate::DockerTestError;

pub use async_trait::async_trait;
use dyn_clone::DynClone;

mod message;
mod nowait;
mod status;

pub(crate) use message::wait_for_message;
pub use message::{MessageSource, MessageWait};
pub use nowait::NoWait;
pub use status::{ExitedWait, RunningWait};

/// Trait to wait for a container to be ready for service.
#[async_trait]
pub trait WaitFor: Send + Sync + DynClone + std::fmt::Debug {
    /// Method implementation should return a future that resolves once the condition
    /// described by the implementing structure is fulfilled. Once this successfully resolves,
    /// the container is marked as ready.
    ///
    // TODO: Implement error propagation with the container id that failed for cleanup
    async fn wait_for_ready(
        &self,
        container: PendingContainer,
    ) -> Result<OperationalContainer, DockerTestError>;
}

dyn_clone::clone_trait_object!(WaitFor);
