//! Represents the multiple phases and variants a docker container exists in dockertest.

mod cleanup;
mod pending;
mod running;

pub(crate) use cleanup::CleanupContainer;
pub use pending::PendingContainer;
pub(crate) use running::HostPortMappings;
pub use running::RunningContainer;

/// Represents an exisiting static external container.
///
// FIXME: does this need to be public?
#[derive(Clone)]
pub struct StaticExternalContainer {
    pub handle: String,
    pub id: String,
}

pub enum CreatedContainer {
    StaticExternal(StaticExternalContainer),
    Pending(PendingContainer),
}

#[cfg(test)]
mod tests {
    use crate::container::{CreatedContainer, PendingContainer, RunningContainer};
    use crate::image::Source;
    use crate::utils::connect_with_local_or_tls_defaults;
    use crate::waitfor::{async_trait, WaitFor};
    use crate::{Composition, DockerTestError};

    use std::sync::{Arc, RwLock};

    #[derive(Clone)]
    struct TestWaitFor {
        invoked: Arc<RwLock<bool>>,
    }

    #[async_trait]
    impl WaitFor for TestWaitFor {
        async fn wait_for_ready(
            &self,
            container: PendingContainer,
        ) -> Result<RunningContainer, DockerTestError> {
            let mut invoked = self.invoked.write().expect("failed to take invoked lock");
            *invoked = true;
            Ok(container.into())
        }
    }

    // Tests that the provided WaitFor trait object is invoked
    // during the start method of Composition
    #[tokio::test]
    async fn test_wait_for_invoked_during_start() {
        let wait_for = TestWaitFor {
            invoked: Arc::new(RwLock::new(false)),
        };

        let wrapped_wait_for = Box::new(wait_for);

        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = "dockertest-rs/hello".to_string();
        let mut composition =
            Composition::with_repository(repository).with_wait_for(wrapped_wait_for.clone());
        composition.container_name = "dockertest_wait_for_invoked_during_start".to_string();

        // Ensure image is present with id populated
        composition
            .image()
            .pull(&client, &Source::Local)
            .await
            .expect("failed to pull image");

        // Create and start the container
        let pending = composition
            .create(&client, None, false)
            .await
            .expect("failed to create container");
        let container = match pending {
            CreatedContainer::Pending(c) => c,
            _ => panic!("expected pending created container"),
        };
        container.start().await.expect("failed to start container");

        let was_invoked = wrapped_wait_for
            .invoked
            .read()
            .expect("failed to get read lock");

        assert!(
            *was_invoked,
            "wait_for trait object was not invoked during startup"
        );
    }
}
