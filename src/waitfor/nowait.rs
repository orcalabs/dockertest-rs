//! `WaitFor` implementation: `NoWait`.

use crate::container::{PendingContainer, RunningContainer};
use crate::waitfor::{async_trait, WaitFor};
use crate::DockerTestError;

/// The NoWait `WaitFor` implementation for containers.
/// This variant does not wait for anything, resolves immediately.
#[derive(Clone)]
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
    use crate::utils::connect_with_local_or_tls_defaults;
    use crate::waitfor::{NoWait, WaitFor};
    use crate::StartPolicy;

    // Tests that WaitFor implementation for NoWait
    #[tokio::test]
    async fn test_no_wait_returns_ok() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let wait = Box::new(NoWait {});

        let container_name = "this_is_a_name".to_string();
        let id = "this_is_an_id".to_string();
        let handle_key = "this_is_a_handle_key";

        let container = PendingContainer::new(
            &container_name,
            &id,
            handle_key,
            StartPolicy::Relaxed,
            wait.clone(),
            client,
        );

        let result = wait.wait_for_ready(container).await;
        assert!(result.is_ok(), "should always return ok with NoWait");

        let container = result.expect("failed to get container");

        assert_eq!(
            container_name,
            container.name(),
            "returned container is not identical"
        );
    }
}
