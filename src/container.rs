//! Represents a docker `Container`.

use crate::waitfor::WaitFor;
use crate::{DockerTestError, StartPolicy};

use bollard::{container::StartContainerOptions, errors::ErrorKind, Docker};

/// Represent a docker container object in a pending phase between
/// it being created on the daemon, but may not be running.
///
/// This object is an implementation detail of `dockertest-rs` and is only
/// publicly exposed due to the public `WaitFor` trait which is responsible
/// of performing the into conversion from `PendingContainer` to `RunningContainer`.
// NOTE: No methods on this structure, nor fields, shall be publicly exposed.
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
    wait: Option<Box<dyn WaitFor>>,
}

/// Represent a docker container in running state and available to the test body.
// NOTE: Fields within this structure are pub(crate) only for testability
#[derive(Clone, Debug)]
pub struct RunningContainer {
    /// The unique docker container identifier assigned at creation.
    pub(crate) id: String,
    /// The generated docker name for this running container.
    pub(crate) name: String,
    pub(crate) ip: std::net::Ipv4Addr,
}

/// A container representation of a pending or running container, that requires us to
/// perform cleanup on it.
///
/// This structure is an implementation detail of dockertest and shall NOT be publicly
/// exposed.
#[derive(Clone, Debug)]
pub(crate) struct CleanupContainer {
    pub(crate) id: String,
}

impl From<PendingContainer> for RunningContainer {
    fn from(container: PendingContainer) -> RunningContainer {
        RunningContainer {
            id: container.id,
            name: container.name,
            ip: std::net::Ipv4Addr::UNSPECIFIED,
        }
    }
}

impl From<PendingContainer> for CleanupContainer {
    fn from(container: PendingContainer) -> CleanupContainer {
        CleanupContainer { id: container.id }
    }
}

impl From<&PendingContainer> for CleanupContainer {
    fn from(container: &PendingContainer) -> CleanupContainer {
        CleanupContainer {
            id: container.id.clone(),
        }
    }
}

impl From<RunningContainer> for CleanupContainer {
    fn from(container: RunningContainer) -> CleanupContainer {
        CleanupContainer { id: container.id }
    }
}

impl RunningContainer {
    /// Return the generated name on the docker container object for this `RunningContainer`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the docker assigned identifier for this `RunningContainer`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Return the IPv4 address for this container on the local docker network adapter.
    /// Use this address to contact the `RunningContainer` in the test body.
    ///
    /// This property is retrieved from the docker daemon prior to entering the test body.
    /// It is cached internally and not updated between invocations. This means that
    /// if the docker container enters an exited state, this function will still return
    /// the original ip assigned to the container.
    ///
    /// If the [ExitedWait] for strategy is employed on the `Composition`, the `RunningContainer`
    /// will, somewhat contradictory to its name, be in an exited status when the test body
    /// is entered. For this scenarion, this function will return [Ipv4Addr::UNSPECIFIED].
    ///
    /// [Ipv4Addr::UNSPECIFIED]: https://doc.rust-lang.org/std/net/struct.Ipv4Addr.html#associatedconstant.UNSPECIFIED
    /// [ExitedWait]: waitfor/struct.ExitedWait.html
    pub fn ip(&self) -> &std::net::Ipv4Addr {
        &self.ip
    }
}

impl PendingContainer {
    /// Creates a new Container object with the given values.
    pub(crate) fn new<T: ToString, R: ToString, H: ToString>(
        name: T,
        id: R,
        handle: H,
        start_policy: StartPolicy,
        wait: Box<dyn WaitFor>,
        client: Docker,
    ) -> PendingContainer {
        PendingContainer {
            client,
            name: name.to_string(),
            id: id.to_string(),
            handle: handle.to_string(),
            wait: Some(wait),
            start_policy,
        }
    }

    /// Run the start command and initiate the WaitFor condition.
    /// Once the PendingContainer is successfully started and the WaitFor condition
    /// has been achived, the RunningContainer is returned.
    pub(crate) async fn start(mut self) -> Result<RunningContainer, DockerTestError> {
        self.client
            .start_container(&self.name, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| match e.kind() {
                ErrorKind::DockerResponseNotFoundError { message } => {
                    let json: Result<serde_json::Value, serde_json::error::Error> =
                        serde_json::from_str(message);
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
    use crate::container::{PendingContainer, RunningContainer};
    use crate::image::{Image, PullPolicy, Source};
    use crate::waitfor::{NoWait, WaitFor};
    use crate::{Composition, DockerTestError, StartPolicy};

    use futures::future::{self, Future};
    use shiplift;
    use std::rc::Rc;
    use std::sync::RwLock;
    use tokio::runtime::current_thread;

    // Tests that we can create a new container with the new method, and
    // that the correct struct members are set.
    #[test]
    fn test_new_container() {
        let client = Rc::new(shiplift::Docker::new());

        let id = "this_is_an_id".to_string();
        let name = "this_is_a_container_name".to_string();
        let handle_key = "this_is_a_handle_key";

        let container = PendingContainer::new(
            &name,
            &id,
            handle_key,
            StartPolicy::Relaxed,
            Rc::new(NoWait {}),
            client,
        );
        assert_eq!(id, container.id, "wrong id set in container creation");
        assert_eq!(name, container.name, "wrong name set in container creation");
        assert_eq!(
            name, container.name,
            "container name getter returns wrong value"
        );
        assert_eq!(
            handle_key, container.handle_key,
            "wrong handle_key set in container creation"
        );
    }

    struct TestWaitFor {
        invoked: Rc<RwLock<bool>>,
    }

    impl WaitFor for TestWaitFor {
        fn wait_for_ready(
            &self,
            container: PendingContainer,
        ) -> Box<dyn Future<Item = RunningContainer, Error = DockerTestError>> {
            let mut invoked = self.invoked.write().expect("failed to take invoked lock");
            *invoked = true;
            Box::new(future::ok(container.into()))
        }
    }
    // Tests that the provided WaitFor trait object is invoked
    // during the start method of Composition
    #[test]
    fn test_wait_for_invoked_during_start() {
        let wait_for = TestWaitFor {
            invoked: Rc::new(RwLock::new(false)),
        };

        let wrapped_wait_for = Rc::new(wait_for);

        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let repository = "hello-world".to_string();

        let source = Source::DockerHub(PullPolicy::IfNotPresent);
        let image = Image::with_repository(&repository);
        let instance = Composition::with_image(image).with_wait_for(wrapped_wait_for.clone());

        let client = Rc::new(shiplift::Docker::new());

        let res = rt.block_on(instance.image().pull(client.clone(), &source));
        assert!(
            res.is_ok(),
            format!("failed to pull image: {}", res.unwrap_err())
        );

        let container = rt
            .block_on(instance.create(client))
            .expect("failed to create container");

        rt.block_on(container.start())
            .expect("failed to start container");

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
