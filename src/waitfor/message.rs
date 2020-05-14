use crate::container::{PendingContainer, RunningContainer};
use crate::waitfor::WaitFor;
use crate::DockerTestError;

use futures::future::{self, Future};
use futures::stream::Stream;
use shiplift::builder::LogsOptions;
use std::sync::atomic::{self, AtomicBool};
use std::sync::Arc;
use std::time::Duration;
use tokio::prelude::FutureExt;

/// The MessageWait `WaitFor` implementation for containers.
/// This variant will wait until the message appears in the requested source.
pub struct MessageWait {
    /// The message to be contained in source.
    pub message: String,
    /// The source to listen for message.
    pub source: MessageSource,
    /// Number of seconds to wait for message. Times out with an error on expire.
    pub timeout: u16,
}

/// The various sources to listen for a message on.
/// Used by `MessageWait`.
#[derive(Clone, Copy)]
pub enum MessageSource {
    /// Listen to the container Stdout.
    Stdout,
    /// Listen to the container Stderr.
    Stderr,
}

impl WaitFor for MessageWait {
    fn wait_for_ready(
        &self,
        container: PendingContainer,
    ) -> Box<dyn Future<Item = RunningContainer, Error = DockerTestError>> {
        let fut = wait_for_message(container, self.source, self.message.clone(), self.timeout);
        Box::new(fut)
    }
}

fn wait_for_message(
    container: PendingContainer,
    source: MessageSource,
    msg: String,
    timeout: u16,
) -> impl Future<Item = RunningContainer, Error = DockerTestError> {
    // Construct LogOptionsBuilder
    let mut options_builder = LogsOptions::builder();
    options_builder.follow(true);
    match source {
        MessageSource::Stdout => options_builder.stdout(true),
        MessageSource::Stderr => options_builder.stderr(true),
    };
    let log_options = options_builder.build();

    let client = shiplift::Docker::new();
    let stream = shiplift::Container::new(&client, container.id.to_string()).logs(&log_options);
    let desired_state = Arc::new(AtomicBool::new(false));
    let ds_takewhile = desired_state.clone();
    let ds_then = desired_state;

    let workfut = stream
        .take_while(move |chunk| {
            let content = chunk.as_string_lossy();
            if content.contains(&msg) {
                ds_takewhile.store(true, atomic::Ordering::SeqCst);
                future::ok(false)
            } else {
                future::ok(true)
            }
        })
        // Discard chunk - how do we propagate error from stream?
        .for_each(move |_| future::ok(()))
        .then(move |res| {
            if let Err(_e) = res {
                // Error occurred while reading stream - not found
                future::Either::B(future::err(DockerTestError::Processing(
                    "error occured on stream".to_string(),
                )))
            } else if ds_then.load(atomic::Ordering::SeqCst) {
                // The message was found in the message source
                future::Either::A(future::ok(container.into()))
            } else {
                // No such message was found, and the stream completed.
                future::Either::B(future::err(DockerTestError::Processing(
                    "stream completed - no message".to_string(),
                )))
            }
        });

    // QUESTION: Shall we handle the err to create a custom error message for timeout?
    workfut
        .timeout(Duration::from_secs(timeout.into()))
        // Have to map the err to get the same return type..
        .map_err(|e| DockerTestError::Processing(format!("{:?}", e)))
}
