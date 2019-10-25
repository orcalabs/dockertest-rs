//! `WaitFor` implementations regarding status changes.

use crate::container::Container;
use crate::waitfor::WaitFor;
use failure::{format_err, Error};
use futures::future::{self, Future};
use futures::stream::Stream;
use std::sync::atomic::{self, AtomicBool};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::timer::Interval;

/// The RunningWait `WaitFor` implementation for containers.
/// This variant will wait until the docker daemon reports the container as running.
pub struct RunningWait {
    /// How many seconds shall there be between each check for running state.
    pub check_interval: i32,
    /// The number of checks to perform before erroring out.
    pub max_checks: i32,
}

/// The ExitedWait `WaitFor` implementation for containers.
/// This variant will wait until the docker daemon reports that the container has exited.
pub struct ExitedWait {
    /// How many seconds shall there be between each check for running state.
    pub check_interval: i32,
    /// The number of checks to perform before erroring out.
    pub max_checks: i32,
}

impl WaitFor for RunningWait {
    fn wait_for_ready(
        &self,
        container: Container,
    ) -> Box<dyn Future<Item = Container, Error = Error>> {
        let liveness_future =
            wait_for_container_state(container, self.check_interval, self.max_checks, |state| {
                state.running
            });

        Box::new(liveness_future)
    }
}

impl WaitFor for ExitedWait {
    fn wait_for_ready(
        &self,
        container: Container,
    ) -> Box<dyn Future<Item = Container, Error = Error>> {
        let liveness_future =
            wait_for_container_state(container, self.check_interval, self.max_checks, |state| {
                !state.running
            });

        Box::new(liveness_future)
    }
}

fn wait_for_container_state(
    container: Container,
    check_interval: i32,
    max_checks: i32,
    container_state_compare: fn(&shiplift::rep::State) -> bool,
) -> impl Future<Item = Container, Error = Error> {
    let client = shiplift::Docker::new();
    let container_name = container.name().to_string();

    let desired_state = Arc::new(AtomicBool::new(false));

    let desired_state_clone = desired_state.clone();
    let desired_state_clone2 = desired_state.clone();

    Interval::new(Instant::now(), Duration::from_millis(check_interval as u64))
        // Limits us to only check status for the given amount of tries
        .take(max_checks as u64)
        .map_err(|e| format_err!("failed to check container liveness: {}", e))
        // While continue checking container status until the desired state is reached.
        // If the desired state is reached we return false to stop the stream.
        .take_while(move |_| {
            let s = desired_state_clone.load(atomic::Ordering::SeqCst);
            if s {
                future::ok(false)
            } else {
                future::ok(true)
            }
        })
        .for_each(move |_| {
            let desired_state_clone3 = desired_state.clone();
            client
                .containers()
                .get(&container_name)
                .inspect()
                .map_err(|e| format_err!("failed to inspect container: {}", e))
                .and_then(move |c| {
                    if container_state_compare(&c.state) {
                        desired_state_clone3.store(true, atomic::Ordering::SeqCst);
                    }

                    future::ok(())
                })
        })
        .then(move |r| {
            // We failed checking the status of the container
            if let Err(e) = r {
                future::Either::B(future::err(format_err!("{}", e)))
            } else {
                let s = desired_state_clone2.load(atomic::Ordering::SeqCst);
                // The desired status has been reached and we can return Ok
                if s {
                    future::Either::A(future::ok(container))
                } else {
                    // The desired status was not reached and we return an error
                    future::Either::B(future::err(format_err!(
                        "container failed to reach desired container state, container name: {}",
                        container.name()
                    )))
                }
            }
        })
}