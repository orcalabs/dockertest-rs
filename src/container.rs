//! Represents a docker `Container`.

use crate::{
    composition::{LogAction, LogOptions},
    static_container::STATIC_CONTAINERS,
    waitfor::{wait_for_message, MessageSource, WaitFor},
    DockerTestError, StartPolicy,
};

use bollard::{
    container::{LogOutput, StartContainerOptions},
    errors::Error,
    models::{PortBinding, PortMap},
    Docker,
};
use futures::StreamExt;
use serde::Serialize;
use tracing::info;

use std::{
    collections::HashMap,
    convert::TryFrom,
    net::{IpAddr, Ipv4Addr},
    str::FromStr,
};

/// Represent a docker container object in a pending phase between
/// it being created on the daemon, but may not be running.
///
/// This object is an implementation detail of `dockertest-rs` and is only
/// publicly exposed due to the public `WaitFor` trait which is responsible
/// of performing the into conversion from `PendingContainer` to `RunningContainer`.
// NOTE: No methods on this structure, nor fields, shall be publicly exposed.
#[derive(Clone)]
pub struct PendingContainer {
    /// The docker client
    pub(crate) client: Docker,

    /// Name of the container, defaults to the repository name of the image.
    pub(crate) name: String,

    /// Id of the running container.
    pub(crate) id: String,

    /// Handle used to interact with the container from the user
    pub(crate) handle: String,

    /// The StartPolicy of this Container, is provided from its Composition.
    pub(crate) start_policy: StartPolicy,

    /// Trait implementing how to wait for the container to startup.
    wait: Option<Box<dyn WaitFor>>,

    /// Wheter this is a static container
    is_static: bool,

    /// Container log options, they are provided by `Composition`.
    pub(crate) log_options: Option<LogOptions>,
}

/// Represent a docker container in running state and available to the test body.
// NOTE: Fields within this structure are pub(crate) only for testability
#[derive(Clone, Debug)]
pub struct RunningContainer {
    pub(crate) client: Docker,
    pub(crate) handle: String,
    /// The unique docker container identifier assigned at creation.
    pub(crate) id: String,
    /// The generated docker name for this running container.
    pub(crate) name: String,
    /// IP address of the container
    pub(crate) ip: std::net::Ipv4Addr,
    /// Published container ports
    pub(crate) ports: HostPortMappings,
    pub(crate) is_static: bool,
    pub(crate) log_options: Option<LogOptions>,
}

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

#[derive(Clone, Debug)]
pub(crate) struct HostPortMappings {
    mappings: HashMap<u32, (Ipv4Addr, u32)>,
}

#[derive(thiserror::Error, Debug, PartialEq, Clone)]
pub(crate) enum HostPortMappingError {
    #[error("failed to extract host port from docker details, malformed ip/protocol key: {0}")]
    HostPortKey(String),
    #[error("port mapping did not contain a host port or host ip")]
    NoMapping,
    #[error("failed to convert host ip/port to appropriate types: {0}")]
    Conversion(String),
}

impl Default for HostPortMappings {
    fn default() -> HostPortMappings {
        HostPortMappings {
            mappings: HashMap::new(),
        }
    }
}

use std::io::{self, Write};

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

impl From<PendingContainer> for RunningContainer {
    fn from(container: PendingContainer) -> RunningContainer {
        RunningContainer {
            client: container.client,
            handle: container.handle,
            id: container.id,
            name: container.name,
            ip: std::net::Ipv4Addr::UNSPECIFIED,
            ports: HostPortMappings::default(),
            is_static: container.is_static,
            log_options: container.log_options,
        }
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

impl TryFrom<PortMap> for HostPortMappings {
    type Error = HostPortMappingError;
    fn try_from(p: PortMap) -> Result<HostPortMappings, Self::Error> {
        let mut map: HashMap<u32, (Ipv4Addr, u32)> = HashMap::new();
        for (host_port_string, ports) in p.into_iter() {
            if let Some(port_bindings) = ports {
                let split: Vec<&str> = host_port_string.split('/').collect();
                // We expect the split to contain "port/protocol" e.g. "8080/tcp"
                // If it does not contain this, we cannot extract the host_port and do not
                // have any fallback and return an early error.
                if split.len() < 2 {
                    return Err(HostPortMappingError::HostPortKey(host_port_string));
                }

                let host_port = u32::from_str(split[0])
                    .map_err(|e| HostPortMappingError::Conversion(e.to_string()))?;

                for binding in port_bindings {
                    if let Some((ip, port)) = from_port_binding(binding)? {
                        map.entry(host_port).or_insert((ip, port));
                    }
                }
            }
        }

        Ok(HostPortMappings { mappings: map })
    }
}

fn from_port_binding(ports: PortBinding) -> Result<Option<(Ipv4Addr, u32)>, HostPortMappingError> {
    match (ports.host_ip, ports.host_port) {
        (Some(ip), Some(port)) => {
            // When 'publish_all_ports' is used 2 port bindings are made for each port, one for
            // ipv4 and one for ipv6.
            // We only want to return a single address/port pair in our API so we skip all ipv6
            // pairs.
            let ip = IpAddr::from_str(&ip)
                .map_err(|e| HostPortMappingError::Conversion(e.to_string()))?;
            match ip {
                IpAddr::V4(ipv4) => {
                    let parsed_port = u32::from_str(&port)
                        .map_err(|e| HostPortMappingError::Conversion(e.to_string()))?;
                    Ok(Some((ipv4, parsed_port)))
                }
                IpAddr::V6(_) => Ok(None),
            }
        }
        _ => Err(HostPortMappingError::NoMapping),
    }
}

impl RunningContainer {
    /// Return the generated name on the docker container object for this `RunningContainer`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the docker assigned identifier for this `RunningContainer`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Return the IPv4 address for this container on the local docker network adapter.
    /// Use this address to contact the `RunningContainer` in the test body.
    ///
    /// This property is retrieved from the docker daemon prior to entering the test body.
    /// It is cached internally and not updated between invocations. This means that
    /// if the docker container enters an exited state, this function will still return
    /// the original ip assigned to the container.
    ///
    /// If the [ExitedWait] for strategy is employed on the `Composition`, the `RunningContainer`
    /// will, somewhat contradictory to its name, be in an exited status when the test body
    /// is entered. For this scenarion, this function will return [Ipv4Addr::UNSPECIFIED].
    ///
    /// On Windows this method always returns `127.0.0.1` due to Windows not supporting using
    /// container IPs outside a container-context.
    ///
    /// [Ipv4Addr::UNSPECIFIED]: https://doc.rust-lang.org/std/net/struct.Ipv4Addr.html#associatedconstant.UNSPECIFIED
    /// [ExitedWait]: crate::waitfor::ExitedWait
    pub fn ip(&self) -> &std::net::Ipv4Addr {
        &self.ip
    }

    /// Returns host ip/port binding for the given container port. Useful in MacOS where there is no
    /// network connectivity between Mac system and containers.
    pub fn host_port(&self, exposed_port: u32) -> Option<&(Ipv4Addr, u32)> {
        self.ports.mappings.get(&exposed_port)
    }

    /// Same as `host_port`, but panics if the mapping could not be found.
    pub fn host_port_unchecked(&self, exposed_port: u32) -> &(Ipv4Addr, u32) {
        self.ports.mappings.get(&exposed_port).unwrap()
    }

    /// Inspect the output of this container and await the presence of a log line.
    ///
    /// # Panics
    /// This function panics if the log message is not present on the log output
    /// within the specified timeout.
    pub async fn assert_message<T>(&self, message: T, source: MessageSource, timeout: u16)
    where
        T: Into<String> + Serialize,
    {
        if let Err(e) = wait_for_message(
            &self.client,
            &self.id,
            &self.handle,
            source,
            message,
            timeout,
        )
        .await
        {
            panic!("{}", e)
        }
    }
}

impl PendingContainer {
    /// Creates a new Container object with the given values.
    // FIXME(veeg): reword the PendingContainer API to be more ergonomic
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<T: ToString, R: ToString, H: ToString>(
        name: T,
        id: R,
        handle: H,
        start_policy: StartPolicy,
        wait: Box<dyn WaitFor>,
        client: Docker,
        is_static: bool,
        log_options: Option<LogOptions>,
    ) -> PendingContainer {
        PendingContainer {
            client,
            name: name.to_string(),
            id: id.to_string(),
            handle: handle.to_string(),
            wait: Some(wait),
            start_policy,
            is_static,
            log_options,
        }
    }

    /// Run the start command and initiate the WaitFor condition.
    /// Once the PendingContainer is successfully started and the WaitFor condition
    /// has been achived, the RunningContainer is returned.
    pub(crate) async fn start(self) -> Result<RunningContainer, DockerTestError> {
        if self.is_static {
            STATIC_CONTAINERS.start(&self).await
        } else {
            self.start_internal().await
        }
    }

    /// Internal start method should only be invoked from the static mod.
    pub(crate) async fn start_internal(mut self) -> Result<RunningContainer, DockerTestError> {
        self.client
            .start_container(&self.name, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| match e {
                Error::DockerResponseNotFoundError { message } => {
                    let json: Result<serde_json::Value, serde_json::error::Error> =
                        serde_json::from_str(message.as_str());
                    match json {
                        Ok(json) => DockerTestError::Startup(format!(
                            "failed to start container due to `{}`",
                            json["message"].as_str().unwrap()
                        )),
                        Err(e) => DockerTestError::Daemon(format!(
                            "daemon json response decode failure: {}",
                            e
                        )),
                    }
                }
                _ => DockerTestError::Daemon(format!("failed to start container: {}", e)),
            })?;

        let waitfor = self.wait.take().unwrap();

        // Issue WaitFor operation
        let res = waitfor.wait_for_ready(self);
        res.await
    }
}

#[cfg(test)]
mod tests {
    use crate::container::{PendingContainer, RunningContainer};
    use crate::image::Source;
    use crate::utils::connect_with_local_or_tls_defaults;
    use crate::waitfor::{async_trait, NoWait, WaitFor};
    use crate::{Composition, DockerTestError, StartPolicy};

    use std::sync::{Arc, RwLock};

    /// Tests `PendingContainer::new` with associated struct member field values.
    #[tokio::test]
    async fn test_new_pending_container() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let id = "this_is_an_id".to_string();
        let name = "this_is_a_container_name".to_string();
        let handle_key = "this_is_a_handle_key";

        let container = PendingContainer::new(
            &name,
            &id,
            handle_key,
            StartPolicy::Relaxed,
            Box::new(NoWait {}),
            client,
            false,
            None,
        );
        assert_eq!(id, container.id, "wrong id set in container creation");
        assert_eq!(name, container.name, "wrong name set in container creation");
        assert_eq!(
            name, container.name,
            "container name getter returns wrong value"
        );
        assert_eq!(
            handle_key, container.handle,
            "wrong handle_key set in container creation"
        );
    }

    #[derive(Clone)]
    struct TestWaitFor {
        invoked: Arc<RwLock<bool>>,
    }

    #[async_trait]
    impl WaitFor for TestWaitFor {
        async fn wait_for_ready(
            &self,
            container: PendingContainer,
        ) -> Result<RunningContainer, DockerTestError> {
            let mut invoked = self.invoked.write().expect("failed to take invoked lock");
            *invoked = true;
            Ok(container.into())
        }
    }

    // Tests that the provided WaitFor trait object is invoked
    // during the start method of Composition
    #[tokio::test]
    async fn test_wait_for_invoked_during_start() {
        let wait_for = TestWaitFor {
            invoked: Arc::new(RwLock::new(false)),
        };

        let wrapped_wait_for = Box::new(wait_for);

        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = "dockertest-rs/hello".to_string();
        let mut composition =
            Composition::with_repository(repository).with_wait_for(wrapped_wait_for.clone());
        composition.container_name = "dockertest_wait_for_invoked_during_start".to_string();

        // Ensure image is present with id populated
        composition
            .image()
            .pull(&client, &Source::Local)
            .await
            .expect("failed to pull image");

        // Create and start the container
        let container = composition
            .create(&client, None, false)
            .await
            .expect("failed to create container")
            .unwrap();
        container.start().await.expect("failed to start container");

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
