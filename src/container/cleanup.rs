//! Represents a container scheduled for cleanup.

use crate::{
    composition::{LogAction, LogOptions},
    container::{OperationalContainer, PendingContainer},
    docker::{ContainerLogSource, Docker, LogEntry},
    waitfor::MessageSource,
    DockerTestError, LogSource,
};
use futures::StreamExt;
use std::io::{self, Write};
use tracing::info;

/// A container representation of a pending or running container, that requires us to
/// perform cleanup on it.
///
/// This structure is an implementation detail of dockertest and shall NOT be publicly
/// exposed.
#[derive(Clone, Debug)]
pub(crate) struct CleanupContainer {
    pub(crate) id: String,
    is_static: bool,
    /// The generated docker name for this container.
    pub(crate) name: String,
    /// Client obtained from `PendingContainer` or `OperationalContainer`, we need it because
    /// we want to call `client.logs` to get container logs.
    pub(crate) client: Docker,
    /// Container log options.
    pub(crate) log_options: Option<LogOptions>,
}

impl CleanupContainer {
    pub(crate) fn is_static(&self) -> bool {
        self.is_static
    }

    /// Handle one log entry.
    async fn handle_log_line(
        &self,
        action: &LogAction,
        entry: LogEntry,
        file: &mut Option<tokio::fs::File>,
    ) -> Result<(), DockerTestError> {
        let write_to_stdout = |message| {
            io::stdout()
                .write(message)
                .map_err(|error| DockerTestError::LogWriteError(format!("stdout: {}", error)))?;
            Ok(())
        };

        let write_to_stderr = |message| {
            io::stderr()
                .write(message)
                .map_err(|error| DockerTestError::LogWriteError(format!("stderr: {}", error)))?;
            Ok(())
        };

        match action {
            // forward-only, print stdout/stderr output to current process stdout/stderr
            LogAction::Forward => match entry.source {
                MessageSource::Stdout => write_to_stdout(&entry.message[..]),
                MessageSource::Stderr => write_to_stderr(&entry.message[..]),
            },
            // forward everything to stderr
            LogAction::ForwardToStdErr => write_to_stderr(&entry.message[..]),
            // forward everything to stdout
            LogAction::ForwardToStdOut => write_to_stdout(&entry.message[..]),
            // forward everything to a file, file should be already opened
            LogAction::ForwardToFile { .. } => {
                use tokio::io::AsyncWriteExt;

                if let Some(ref mut file) = file {
                    file.write(&entry.message[..])
                        .await
                        .map_err(|error| {
                            DockerTestError::LogWriteError(format!(
                                "unable to write to log file: {}",
                                error
                            ))
                        })
                        .map(|_| ())
                } else {
                    Err(DockerTestError::LogWriteError(
                        "log file should not be None".to_string(),
                    ))
                }
            }
        }
    }

    /// Handle container logs.
    pub(crate) async fn handle_log(
        &self,
        action: &LogAction,
        source: &LogSource,
    ) -> Result<(), DockerTestError> {
        // check if we need to capture stderr and/or stdout
        let should_log_stderr = match source {
            LogSource::StdErr => true,
            LogSource::StdOut => false,
            LogSource::Both => true,
        };

        let should_log_stdout = match source {
            LogSource::StdErr => false,
            LogSource::StdOut => true,
            LogSource::Both => true,
        };

        let source = ContainerLogSource {
            log_stderr: should_log_stderr,
            log_stdout: should_log_stdout,
            ..Default::default()
        };

        info!("Trying to get logs from container: id={}", self.id);
        let mut stream = self.client.container_logs(&self.name, source);

        // let's open file if need it, we are doing this because we dont want to open
        // file in every log reading iteration
        let mut file = match action {
            LogAction::ForwardToFile { path } => {
                let filepath = format!("{}/{}", path, self.name);
                // try to create file, bail if we cannot create file
                tokio::fs::File::create(filepath)
                    .await
                    .map(Some)
                    .map_err(|error| {
                        DockerTestError::LogWriteError(format!(
                            "unable to create log file: {}",
                            error
                        ))
                    })
            }
            _ => Ok(None),
        }?;

        while let Some(data) = stream.next().await {
            match data {
                Ok(line) => self.handle_log_line(action, line, &mut file).await?,
                Err(error) => {
                    return Err(DockerTestError::LogWriteError(format!(
                        "unable to read docker log: {}",
                        error
                    )))
                }
            }
        }

        Ok(())
    }
}

impl From<PendingContainer> for CleanupContainer {
    fn from(container: PendingContainer) -> CleanupContainer {
        CleanupContainer {
            id: container.id,
            is_static: container.is_static,
            client: container.client,
            log_options: container.log_options,
            name: container.name,
        }
    }
}

impl From<&PendingContainer> for CleanupContainer {
    fn from(container: &PendingContainer) -> CleanupContainer {
        CleanupContainer {
            id: container.id.clone(),
            is_static: container.is_static,
            client: container.client.clone(),
            log_options: container.log_options.clone(),
            name: container.name.clone(),
        }
    }
}

impl From<OperationalContainer> for CleanupContainer {
    fn from(container: OperationalContainer) -> CleanupContainer {
        CleanupContainer {
            id: container.id,
            is_static: container.is_static,
            client: container.client,
            log_options: container.log_options,
            name: container.name,
        }
    }
}

impl From<&OperationalContainer> for CleanupContainer {
    fn from(container: &OperationalContainer) -> CleanupContainer {
        CleanupContainer {
            id: container.id.clone(),
            is_static: container.is_static,
            client: container.client.clone(),
            log_options: container.log_options.clone(),
            name: container.name.clone(),
        }
    }
}
