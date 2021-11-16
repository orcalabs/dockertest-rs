//! Represents a container scheduled for cleanup.

use crate::{
    composition::{LogAction, LogOptions},
    container::{PendingContainer, RunningContainer},
    DockerTestError,
};

use bollard::{container::LogOutput, Docker};
use futures::StreamExt;
use tracing::info;

use std::io::{self, Write};

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
    /// Client obtained from `PendingContainer` or `RunningContainer`, we need it because
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
        output: LogOutput,
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
            LogAction::Forward => match output {
                LogOutput::StdOut { message } => write_to_stdout(&message[..]),
                LogOutput::StdErr { message } => write_to_stderr(&message[..]),
                LogOutput::StdIn { .. } | LogOutput::Console { .. } => Ok(()),
            },
            // forward everything to stderr
            LogAction::ForwardToStdErr => match output {
                LogOutput::StdOut { message } | LogOutput::StdErr { message } => {
                    write_to_stderr(&message[..])
                }
                LogOutput::StdIn { .. } | LogOutput::Console { .. } => Ok(()),
            },
            // forward everything to stdout
            LogAction::ForwardToStdOut => match output {
                LogOutput::StdOut { message } | LogOutput::StdErr { message } => {
                    write_to_stdout(&message[..])
                }
                LogOutput::StdIn { .. } | LogOutput::Console { .. } => Ok(()),
            },
            // forward everything to a file, file should be already opened
            LogAction::ForwardToFile { .. } => match output {
                LogOutput::StdOut { message } | LogOutput::StdErr { message } => {
                    use tokio::io::AsyncWriteExt;

                    if let Some(ref mut file) = file {
                        file.write(&message[..])
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
                LogOutput::StdIn { .. } | LogOutput::Console { .. } => Ok(()),
            },
        }
    }

    /// Handle container logs.
    pub(crate) async fn handle_log(
        &self,
        action: &LogAction,
        should_log_stderr: bool,
        should_log_stdout: bool,
    ) -> Result<(), DockerTestError> {
        use bollard::container::LogsOptions;

        let options = Some(LogsOptions::<String> {
            stdout: should_log_stdout,
            stderr: should_log_stderr,
            ..Default::default()
        });

        info!("Trying to get logs from container: id={}", self.id);
        let mut stream = self.client.logs(&self.name, options);

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

impl From<RunningContainer> for CleanupContainer {
    fn from(container: RunningContainer) -> CleanupContainer {
        CleanupContainer {
            id: container.id,
            is_static: container.is_static,
            client: container.client,
            log_options: container.log_options,
            name: container.name,
        }
    }
}

impl From<&RunningContainer> for CleanupContainer {
    fn from(container: &RunningContainer) -> CleanupContainer {
        CleanupContainer {
            id: container.id.clone(),
            is_static: container.is_static,
            client: container.client.clone(),
            log_options: container.log_options.clone(),
            name: container.name.clone(),
        }
    }
}
