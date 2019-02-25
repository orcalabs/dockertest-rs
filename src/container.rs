use crate::error::DockerError;
use crate::wait_for::WaitFor;
use failure::Error;
use futures::future::Future;
use futures::Async;
use shiplift;
use shiplift::builder::RmContainerOptions;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio::timer::Delay;

/// Represents a running container
#[derive(Clone)]
pub struct Container {
    /// The docker client
    client: Rc<shiplift::Docker>,

    /// The provided or default WaitFor trait object.
    /// This will be called repeadetly when waiting for the
    /// container to start.
    /// Once the WaitFor method returns true, the container
    /// will be assumed to be ready for service.
    wait: Rc<dyn WaitFor>,

    /// Name of the containers, defaults to the
    /// repository name of the image.
    name: String,

    /// Id of the running container.
    id: String,
}

impl Container {
    /// Creates a new Container object with the given
    /// values.
    pub(crate) fn new(
        name: String,
        client: Rc<shiplift::Docker>,
        wait: Rc<dyn WaitFor>,
        id: String,
    ) -> Container {
        Container {
            client,
            wait,
            name,
            id,
        }
    }

    /// Returns which host port the given container port is mapped to.
    pub fn host_port(&self, _container_port: u32) -> u32 {
        0
    }

    /// Returns the name of container
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Forcefully removes the container and consumes self.
    pub(crate) fn remove(self) -> impl Future<Item = (), Error = DockerError> {
        let ops = RmContainerOptions::builder().force(true).build();
        shiplift::Container::new(&self.client, self.id)
            .remove(ops)
            .map_err(|e| DockerError::daemon(format!("failed to remove container: {}", e)))
    }

    /// Schedules a task on the default tokio runtime,
    /// which wakes up after 1 second and notifies the current task.
    fn schedule_delayed_notification(&self) {
        let sleep_time = Instant::now() + Duration::from_millis(1000);

        let task = futures::task::current();
        let sleep_fut = Delay::new(sleep_time)
            .map_err(|_e| eprintln!("tokio timer failed"))
            .and_then(move |_| {
                task.notify();
                Ok(())
            });

        tokio::spawn(sleep_fut);
    }
}

impl Future for Container {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> futures::Poll<Self::Item, Self::Error> {
        if self.wait.wait_for_ready(self) {
            Ok(Async::Ready(()))
        } else {
            self.schedule_delayed_notification();
            Ok(Async::NotReady)
        }
    }
}
