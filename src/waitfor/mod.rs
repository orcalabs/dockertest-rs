//! Contains `WaitFor` trait used to determine when a PendingContainer has started
//! and all the default implementations of it.

use crate::container::{OperationalContainer, PendingContainer};
use crate::docker::ContainerState;
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

    /// What state the container is expected to be in after completing the `wait_for_ready` method,
    /// defaulting to the `ContainerState::Running` state.
    /// NOTE: This is only relevant for the container state api on 'OperationalContainer' (start, stop,
    /// kill) as we deny certain operations based on the assumed container state.
    fn expected_state(&self) -> ContainerState {
        ContainerState::Running
    }
}

dyn_clone::clone_trait_object!(WaitFor);
