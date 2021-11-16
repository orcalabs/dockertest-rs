//! Represents a created container, in transit to become a RunningContainer.

use crate::{
    composition::LogOptions, container::RunningContainer, static_container::STATIC_CONTAINERS,
    waitfor::WaitFor, DockerTestError, StartPolicy,
};

use bollard::{container::StartContainerOptions, errors::Error, Docker};

/// Represent a docker container object in a pending phase between
/// it being created on the daemon, but may not be running.
///
/// This object is an implementation detail of `dockertest-rs` and is only
/// publicly exposed due to the public `WaitFor` trait which is responsible
/// of performing the into conversion from `PendingContainer` to `RunningContainer`.
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

    /// Container log options, they are provided by `Composition`.
    pub(crate) log_options: Option<LogOptions>,
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
        is_static: bool,
        log_options: Option<LogOptions>,
    ) -> PendingContainer {
        PendingContainer {
            client,
            name: name.to_string(),
            id: id.to_string(),
            handle: handle.to_string(),
            wait: Some(wait),
            start_policy,
            is_static,
            log_options,
        }
    }

    /// Run the start command and initiate the WaitFor condition.
    /// Once the PendingContainer is successfully started and the WaitFor condition
    /// has been achived, the RunningContainer is returned.
    pub(crate) async fn start(self) -> Result<RunningContainer, DockerTestError> {
        if self.is_static {
            STATIC_CONTAINERS.start(&self).await
        } else {
            self.start_internal().await
        }
    }

    /// Internal start method should only be invoked from the static mod.
    pub(crate) async fn start_internal(mut self) -> Result<RunningContainer, DockerTestError> {
        self.client
            .start_container(&self.name, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| match e {
                Error::DockerResponseNotFoundError { message } => {
                    let json: Result<serde_json::Value, serde_json::error::Error> =
                        serde_json::from_str(message.as_str());
                    match json {
                        Ok(json) => DockerTestError::Startup(format!(
                            "failed to start container due to `{}`",
                            json["message"].as_str().unwrap()
                        )),
                        Err(e) => DockerTestError::Daemon(format!(
                            "daemon json response decode failure: {}",
                            e
                        )),
                    }
                }
                _ => DockerTestError::Daemon(format!("failed to start container: {}", e)),
            })?;

        let waitfor = self.wait.take().unwrap();

        // Issue WaitFor operation
        let res = waitfor.wait_for_ready(self);
        res.await
    }
}

#[cfg(test)]
mod tests {
    use crate::container::PendingContainer;
    use crate::utils::connect_with_local_or_tls_defaults;
    use crate::waitfor::NoWait;
    use crate::StartPolicy;

    /// Tests `PendingContainer::new` with associated struct member field values.
    #[tokio::test]
    async fn test_new_pending_container() {
        let client = connect_with_local_or_tls_defaults().unwrap();
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
            false,
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
