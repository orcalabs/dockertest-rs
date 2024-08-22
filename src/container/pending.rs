//! Represents a created container, in transit to become a OperationalContainer.

use crate::{
    composition::{LogOptions, StaticManagementPolicy},
    container::OperationalContainer,
    docker::{ContainerState, Docker},
    waitfor::WaitFor,
    DockerTestError, StartPolicy,
};

/// Represent a docker container object in a pending phase between
/// it being created on the daemon, but may not be running.
///
/// This object is an implementation detail of `dockertest-rs` and is only
/// publicly exposed due to the public `WaitFor` trait which is responsible
/// of performing the into conversion from `PendingContainer` to `OperationalContainer`.
// NOTE: No methods on this structure, nor fields, shall be publicly exposed.
#[derive(Clone)]
pub struct PendingContainer {
    /// The docker client
    pub(crate) client: Docker,

    /// Name of the container, defaults to the repository name of the image.
    pub(crate) name: String,

    /// Id of the running container.
    pub(crate) id: String,

    /// Handle used to interact with the container from the user
    pub(crate) handle: String,

    /// The StartPolicy of this Container, is provided from its Composition.
    pub(crate) start_policy: StartPolicy,

    /// Trait implementing how to wait for the container to startup.
    pub(crate) wait: Option<Box<dyn WaitFor>>,

    /// Wheter this is a static container
    pub(crate) is_static: bool,

    /// The StaticManagementPolicy of this container if any exists
    pub(crate) static_management_policy: Option<StaticManagementPolicy>,

    /// Container log options, they are provided by `Composition`.
    pub(crate) log_options: Option<LogOptions>,

    /// The containers' expected state after executing its `WaitFor` implementation.
    pub(crate) expected_state: ContainerState,
}

impl PendingContainer {
    /// Creates a new Container object with the given values.
    // FIXME(veeg): reword the PendingContainer API to be more ergonomic
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<T: ToString, R: ToString, H: ToString>(
        name: T,
        id: R,
        handle: H,
        start_policy: StartPolicy,
        wait: Box<dyn WaitFor>,
        client: Docker,
        static_management_policy: Option<StaticManagementPolicy>,
        log_options: Option<LogOptions>,
    ) -> PendingContainer {
        PendingContainer {
            client,
            name: name.to_string(),
            id: id.to_string(),
            handle: handle.to_string(),
            start_policy,
            is_static: static_management_policy.is_some(),
            static_management_policy,
            log_options,
            expected_state: wait.expected_state(),
            wait: Some(wait),
        }
    }

    /// Run the start command and initiate the WaitFor condition.
    /// Once the PendingContainer is successfully started and the WaitFor condition
    /// has been achived, the OperationalContainer is returned.
    pub(crate) async fn start(self) -> Result<OperationalContainer, DockerTestError> {
        // TODO: dont clone
        let client = self.client.clone();
        client.start_container(self).await
    }

    // Internal start method should only be invoked from the static mod.
    // TODO: isolate to static mod only
    pub(crate) async fn start_inner(self) -> Result<OperationalContainer, DockerTestError> {
        // TODO: dont clone
        let client = self.client.clone();
        client.start_container_inner(self).await
    }
}

#[cfg(test)]
mod tests {
    use crate::container::PendingContainer;
    use crate::docker::Docker;
    use crate::waitfor::NoWait;
    use crate::StartPolicy;

    /// Tests `PendingContainer::new` with associated struct member field values.
    #[tokio::test]
    async fn test_new_pending_container() {
        let client = Docker::new().unwrap();
        let id = "this_is_an_id".to_string();
        let name = "this_is_a_container_name".to_string();
        let handle_key = "this_is_a_handle_key";

        let container = PendingContainer::new(
            &name,
            &id,
            handle_key,
            StartPolicy::Relaxed,
            Box::new(NoWait {}),
            client,
            None,
            None,
        );
        assert_eq!(id, container.id, "wrong id set in container creation");
        assert_eq!(name, container.name, "wrong name set in container creation");
        assert_eq!(
            name, container.name,
            "container name getter returns wrong value"
        );
        assert_eq!(
            handle_key, container.handle,
            "wrong handle_key set in container creation"
        );
    }
}
