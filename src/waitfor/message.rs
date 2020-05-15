use crate::container::{PendingContainer, RunningContainer};
use crate::waitfor::{async_trait, WaitFor};
use crate::DockerTestError;

use bollard::container::{LogOutput, LogsOptions};
use futures::stream::StreamExt;
use std::sync::atomic::{self, AtomicBool};
use std::sync::Arc;
use tokio::{time, time::Duration};
use tracing::{event, Level};

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

#[async_trait]
impl WaitFor for MessageWait {
    async fn wait_for_ready(
        &self,
        container: PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        wait_for_message(container, self.source, self.message.clone(), self.timeout).await
    }
}

async fn wait_for_message(
    container: PendingContainer,
    source: MessageSource,
    msg: String,
    timeout: u16,
) -> Result<RunningContainer, DockerTestError> {
    // Construct LogOptions
    let mut log_options = LogsOptions {
        follow: true,
        ..Default::default()
    };
    match source {
        MessageSource::Stdout => log_options.stdout = true,
        MessageSource::Stderr => log_options.stderr = true,
    };
    let log_options = Some(log_options);

    // Construct remaining variables
    let client = container.client.clone();
    let s1 = Arc::new(AtomicBool::new(false));
    let s2 = s1.clone();

    // Construct the stream
    let stream = client.logs(&container.id, log_options);

    // Work configuration
    let work_fut = async {
        stream
            .take_while(move |chunk| {
                match chunk {
                    Ok(chunk) => {
                        // Extract the String from LogOutput variants
                        let content = match chunk {
                            LogOutput::StdErr { message } => Some(message),
                            LogOutput::StdOut { message } => Some(message),
                            LogOutput::StdIn { message: _ } => None,
                            LogOutput::Console { message: _ } => None,
                        };
                        match content {
                            Some(content) if content.contains(&msg) => {
                                s1.store(true, atomic::Ordering::SeqCst);
                                futures::future::ready(false)
                            }
                            _ => futures::future::ready(true),
                        }
                    }
                    Err(_) => futures::future::ready(false),
                }
            })
            .collect::<Vec<_>>()
            .await
    };

    match time::timeout(Duration::from_secs(timeout.into()), work_fut).await {
        Ok(_) => {
            if s2.load(atomic::Ordering::SeqCst) {
                Ok(container.into())
            } else {
                Err(DockerTestError::Startup(
                    "message waitfor not triggered".to_string(),
                ))
            }
        }
        Err(_) => {
            event!(Level::WARN, "awaiting container message timed out");
            Err(DockerTestError::Startup(
                "awaiting container message timed out".to_string(),
            ))
        }
    }
}
