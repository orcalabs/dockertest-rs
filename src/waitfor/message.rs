use crate::container::{OperationalContainer, PendingContainer};
use crate::docker::{ContainerLogSource, Docker};
use crate::waitfor::{async_trait, WaitFor};
use crate::DockerTestError;
use futures::StreamExt;
use serde::Serialize;
use std::sync::atomic::{self, AtomicBool};
use std::sync::Arc;
use tokio::{time, time::Duration};
use tracing::{event, Level};

/// The MessageWait `WaitFor` implementation for containers.
/// This variant will wait until the message appears in the requested source.
#[derive(Clone, Debug)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    ) -> Result<OperationalContainer, DockerTestError> {
        pending_container_wait_for_message(
            container,
            self.source,
            self.message.clone(),
            self.timeout,
        )
        .await
    }
}

async fn pending_container_wait_for_message(
    container: PendingContainer,
    source: MessageSource,
    msg: String,
    timeout: u16,
) -> Result<OperationalContainer, DockerTestError> {
    let client = &container.client;
    match wait_for_message(
        client,
        &container.id,
        &container.handle,
        source,
        msg,
        timeout,
    )
    .await
    {
        Ok(_) => Ok(container.into()),
        Err(e) => Err(e),
    }
}

pub(crate) async fn wait_for_message<T>(
    client: &Docker,
    container_id: &str,
    handle: &str,
    source: MessageSource,
    msg: T,
    timeout: u16,
) -> Result<(), DockerTestError>
where
    T: Into<String> + Serialize,
{
    let s1 = Arc::new(AtomicBool::new(false));
    let s2 = s1.clone();
    let msg_clone1: String = msg.into();
    let msg_clone2: String = msg_clone1.clone();

    let mut source: ContainerLogSource = source.into();
    source.follow = true;

    let stream = client.container_logs(container_id, source);

    let log_stream_future = async {
        stream
            .take_while(move |message| match message {
                Ok(message) => {
                    if String::from_utf8(message.message.to_vec())
                        .unwrap()
                        .contains(&msg_clone1)
                    {
                        s1.store(true, atomic::Ordering::SeqCst);
                        futures::future::ready(false)
                    } else {
                        futures::future::ready(true)
                    }
                }
                Err(_) => futures::future::ready(false),
            })
            .collect::<Vec<_>>()
            .await
    };

    match time::timeout(Duration::from_secs(timeout.into()), log_stream_future).await {
        Ok(_) => {
            if s2.load(atomic::Ordering::SeqCst) {
                Ok(())
            } else {
                Err(DockerTestError::Startup(
                   format!("container `{}` ended log stream (terminated) before waitfor message triggered: `{}`", handle, msg_clone2),
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
