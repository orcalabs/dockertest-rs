//! `WaitFor` implementations regarding status changes.

use crate::container::{PendingContainer, RunningContainer};
use crate::waitfor::{async_trait, WaitFor};
use crate::DockerTestError;

use bollard::container::InspectContainerOptions;
use bollard::models::ContainerState;
use tokio::time::{interval, Duration};

/// The RunningWait `WaitFor` implementation for containers.
/// This variant will wait until the docker daemon reports the container as running.
#[derive(Clone, Debug)]
pub struct RunningWait {
    /// How many seconds shall there be between each check for running state.
    pub check_interval: u64,
    /// The number of checks to perform before erroring out.
    pub max_checks: u64,
}

/// The ExitedWait `WaitFor` implementation for containers.
/// This variant will wait until the docker daemon reports that the container has exited.
#[derive(Clone, Debug)]
pub struct ExitedWait {
    /// How many seconds shall there be between each check for running state.
    pub check_interval: u64,
    /// The number of checks to perform before erroring out.
    pub max_checks: u64,
}

#[async_trait]
impl WaitFor for RunningWait {
    async fn wait_for_ready(
        &self,
        container: PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        wait_for_container_state(container, self.check_interval, self.max_checks, |state| {
            state.running.unwrap()
        })
        .await
    }
}

#[async_trait]
impl WaitFor for ExitedWait {
    async fn wait_for_ready(
        &self,
        container: PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        wait_for_container_state(container, self.check_interval, self.max_checks, |state| {
            !state.running.unwrap()
        })
        .await
    }
}

async fn wait_for_container_state(
    container: PendingContainer,
    check_interval: u64,
    max_checks: u64,
    container_state_compare: fn(&ContainerState) -> bool,
) -> Result<RunningContainer, DockerTestError> {
    let client = &container.client;

    let mut started = false;
    let mut num_checks = 0;

    // Periodically check container state in an interval.
    // At one point in the future, this check will time out with an error.
    // Once the desired state has been fulfilled within the time out period,
    // the operation returns successfully.

    let mut interval = interval(Duration::from_secs(check_interval));
    loop {
        if num_checks >= max_checks {
            break;
        }

        started = if let Ok(c) = client
            .inspect_container(&container.name, None::<InspectContainerOptions>)
            .await
        {
            container_state_compare(&c.clone().state.unwrap())
        } else {
            false
        };

        if started {
            break;
        }

        num_checks += 1;
        interval.tick().await;
    }

    match started {
        false => Err(DockerTestError::Startup(
            "status waitfor is not triggered".to_string(),
        )),
        true => Ok(container.into()),
    }
}
