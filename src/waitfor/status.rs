//! `WaitFor` implementations regarding status changes.

use crate::container::{PendingContainer, RunningContainer};
use crate::waitfor::{async_trait, WaitFor};
use crate::DockerTestError;

use bollard::container::{InspectContainerOptions, State};
use futures::stream::StreamExt;
use std::sync::atomic::{self, AtomicBool};
use std::sync::Arc;
use tokio::time::{interval, timeout, Duration};
use tracing::{event, Level};

/// The RunningWait `WaitFor` implementation for containers.
/// This variant will wait until the docker daemon reports the container as running.
#[derive(Clone)]
pub struct RunningWait {
    /// How many seconds shall there be between each check for running state.
    pub check_interval: u64,
    /// The number of checks to perform before erroring out.
    pub max_checks: u64,
}

/// The ExitedWait `WaitFor` implementation for containers.
/// This variant will wait until the docker daemon reports that the container has exited.
#[derive(Clone)]
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
            state.running
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
            !state.running
        })
        .await
    }
}

async fn wait_for_container_state(
    container: PendingContainer,
    check_interval: u64,
    max_checks: u64,
    container_state_compare: fn(&State) -> bool,
) -> Result<RunningContainer, DockerTestError> {
    let client = &container.client;

    let s1 = Arc::new(AtomicBool::new(false));
    let s2 = s1.clone();

    // Periodically check container state in an interval.
    // At one point in the future, this check will time out with an error.
    // Once the desired state has been fulfilled within the time out period,
    // the operation returns successfully.

    // Double fucking copy due to FnMut capturing scope and async return values
    let c1 = client.clone();
    let n1 = container.name.clone();

    // Perform the operation every check_interval until we get the desired state
    let check_fut = async {
        interval(Duration::from_secs(check_interval))
            .take_while(move |_| {
                let n2 = n1.clone();
                let c2 = c1.clone();
                let s3 = s1.clone();
                async move {
                    let val: bool = match c2
                        .inspect_container(&n2, None::<InspectContainerOptions>)
                        .await
                    {
                        Ok(container) if container_state_compare(&container.state) => {
                            s3.store(true, atomic::Ordering::SeqCst);
                            false
                        }
                        _ => true,
                    };

                    val
                }
            })
            .collect::<Vec<_>>()
            .await
    };

    // Run the check operation for a specified time period, aborting if it
    // does not complete in the alloted time.
    match timeout(Duration::from_secs(check_interval * max_checks), check_fut).await {
        Ok(_) => {
            if s2.load(atomic::Ordering::SeqCst) {
                Ok(container.into())
            } else {
                Err(DockerTestError::Startup(
                    "status waitfor is not triggered".to_string(),
                ))
            }
        }
        Err(_) => {
            event!(Level::WARN, "awaiting container state timed out");
            Err(DockerTestError::Startup(
                "awaiting container state timed out".to_string(),
            ))
        }
    }
}
