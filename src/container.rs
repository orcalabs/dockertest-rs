//! Represents a docker `Container`.

use crate::error::DockerError;
use crate::waitfor::WaitFor;
use crate::StartPolicy;
use futures::future::Future;
use std::rc::Rc;

/// Represent a docker container object in a pending phase between
/// it being created on the daemon, but may not be running.
///
/// This object is an implementation detail of `dockertest-rs` and is only
/// publicly exposed due to the public `WaitFor` trait which is responsible
/// of performing the into conversion from `PendingContainer` to `RunningContainer`.
// No methods on this structure, not fields, shall be publicly exposed.
#[derive(Clone)]
pub struct PendingContainer {
    /// The docker client
    client: Rc<shiplift::Docker>,

    /// Name of the container, defaults to the repository name of the image.
    pub(crate) name: String,

    /// Id of the running container.
    pub(crate) id: String,

    /// The key used to retrieve the handle to this container from DockerTest.
    /// The user can set this directly by specifying container_name in the Composition builder.
    /// Defaults to the repository name of the containers image.
    handle_key: String,

    /// The StartPolicy of this Container, is provided from its Composition.
    pub(crate) start_policy: StartPolicy,

    /// Trait implementing how to wait for the container to startup.
    wait_for: Rc<dyn WaitFor>,
}

/// Represent a docker container in running state and available to the test body.
// Fields within this structure are pub(crate) only for testability
#[derive(Clone, Debug)]
pub struct RunningContainer {
    /// The unique docker container identifier assigned at creation.
    pub(crate) id: String,
    /// The generated docker name for this running container.
    pub(crate) name: String,
}

/// A Container representation of a created or started container, that requires us to
/// perform cleanup on it.
///
/// This structure is an implementation detail of `dockertest-rs` and shall NOT be publicly
/// exposed.
#[derive(Clone, Debug)]
pub(crate) struct CleanupContainer {
    pub id: String,
}

impl From<PendingContainer> for RunningContainer {
    fn from(container: PendingContainer) -> RunningContainer {
        RunningContainer {
            id: container.id,
            name: container.name,
        }
    }
}

impl From<PendingContainer> for CleanupContainer {
    fn from(container: PendingContainer) -> CleanupContainer {
        CleanupContainer { id: container.id }
    }
}

impl From<RunningContainer> for CleanupContainer {
    fn from(container: RunningContainer) -> CleanupContainer {
        CleanupContainer { id: container.id }
    }
}

impl RunningContainer {
    /// Returns which host port the given container port is mapped to.
    pub fn host_port(&self, container_port: u32) -> u32 {
        // TODO: Implement host-port mapping.
        container_port
    }

    /// Return the generated name on the docker container object for this `RunningContainer`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the docker assigned identifier for this `RunningContainer`.
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl PendingContainer {
    /// Creates a new Container object with the given values.
    pub(crate) fn new<T: ToString, R: ToString, S: ToString>(
        name: T,
        id: R,
        handle: S,
        start_policy: StartPolicy,
        wait_for: Rc<dyn WaitFor>,
        client: Rc<shiplift::Docker>,
    ) -> PendingContainer {
        PendingContainer {
            client,
            name: name.to_string(),
            id: id.to_string(),
            handle_key: handle.to_string(),
            wait_for,
            start_policy,
        }
    }

    /// Run the start command and initiate the WaitFor condition.
    /// Once the PendingContainer is successfully started and the WaitFor condition
    /// has been achived, the RunningContainer is returned.
    pub(crate) fn start(self) -> impl Future<Item = RunningContainer, Error = DockerError> {
        let wait_for_clone = self.wait_for.clone();
        let self_clone = self.clone();
        self.client
            .containers()
            .get(&self.name)
            .start()
            .map_err(|e| DockerError::startup(format!("failed to start container: {}", e)))
            .and_then(move |_| {
                wait_for_clone
                    .wait_for_ready(self_clone)
                    .map_err(|e| {
                        DockerError::startup(format!(
                            "failed to wait for container to be ready: {}",
                            e
                        ))
                    })
                    .map(|_| ())
            })
    }

    /// Returns the name of container.
    // QUESTION: What name?
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the handle_key of this container.
    pub(crate) fn handle_key(&self) -> &str {
        &self.handle_key
    }

    /// Returns a copy of the StartPolicy.
    pub(crate) fn start_policy(&self) -> StartPolicy {
        self.start_policy.clone()
    }

    /// Returns the id of container.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Forcefully removes the container.
    /// This will leave the container object in an invalid state after
    /// this method is invoked.
    pub(crate) fn remove(&self) -> impl Future<Item = (), Error = DockerError> {
        let ops = RmContainerOptions::builder().force(true).build();
        shiplift::Container::new(&self.client, self.id.clone())
            .remove(ops)
            .map_err(|e| DockerError::daemon(format!("failed to remove container: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use crate::container::Container;
    use crate::image::{Image, PullPolicy, Source};
    use crate::waitfor::{NoWait, WaitFor};
    use crate::{Composition, StartPolicy};
    use failure::Error;
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

        let container = Container::new(
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
            name,
            container.name(),
            "container name getter returns wrong value"
        );
        assert_eq!(
            handle_key, container.handle_key,
            "wrong handle_key set in container creation"
        );
        assert_eq!(
            handle_key,
            container.handle_key(),
            "handle_key getter returns wrong value"
        );
    }

    struct TestWaitFor {
        invoked: Rc<RwLock<bool>>,
    }

    impl WaitFor for TestWaitFor {
        fn wait_for_ready(
            &self,
            container: Container,
        ) -> Box<dyn Future<Item = Container, Error = Error>> {
            let mut invoked = self.invoked.write().expect("failed to take invoked lock");
            *invoked = true;
            Box::new(future::ok(container))
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
