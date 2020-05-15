//! `WaitFor` implementation: `NoWait`.

use crate::container::{PendingContainer, RunningContainer};
use crate::waitfor::{async_trait, WaitFor};
use crate::DockerTestError;

/// The NoWait `WaitFor` implementation for containers.
/// This variant does not wait for anything, resolves immediately.
pub struct NoWait {}

#[async_trait]
impl WaitFor for NoWait {
    async fn wait_for_ready(
        &self,
        container: PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        Ok(container.into())
    }
}

#[cfg(test)]
mod tests {
    use crate::container::PendingContainer;
    use crate::waitfor::{NoWait, WaitFor};
    use crate::StartPolicy;

    use shiplift;
    use std::rc::Rc;
    use tokio::runtime::current_thread;

    // Tests that WaitFor implementation for NoWait
    #[test]
    fn test_no_wait_returns_ok() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let wait = Rc::new(NoWait {});

        let container_name = "this_is_a_name".to_string();
        let id = "this_is_an_id".to_string();
        let handle_key = "this_is_a_handle_key";

        let container = PendingContainer::new(
            &container_name,
            &id,
            handle_key,
            StartPolicy::Relaxed,
            wait.clone(),
            Rc::new(shiplift::Docker::new()),
        );

        let res = rt.block_on(wait.wait_for_ready(container));
        assert!(res.is_ok(), "should always return ok with NoWait");

        let container = res.expect("failed to get container");

        assert_eq!(
            container_name,
            container.name(),
            "returned container is not identical"
        );
    }
}
