use super::Docker;
use crate::{
    composition::Composition,
    container::{CleanupContainer, HostPortMappings},
    static_container::STATIC_CONTAINERS,
    waitfor::MessageSource,
    DockerTestError, OperationalContainer, PendingContainer,
};
use bollard::{
    container::{
        InspectContainerOptions, KillContainerOptions, LogOutput, RemoveContainerOptions,
        StartContainerOptions, StopContainerOptions,
    },
    errors::Error,
    network::DisconnectNetworkOptions,
    secret::ContainerInspectResponse,
};
use bytes::Bytes;
use futures::StreamExt;
use futures::{future::join_all, Stream};
use std::{
    convert::TryFrom,
    sync::{Arc, Mutex},
};
use tracing::{event, Level};

pub struct ContainerInfo {
    pub ip: std::net::Ipv4Addr,
    pub ports: HostPortMappings,
}

/// All possible states a container can be in
#[derive(Debug, PartialEq, Eq, Clone, Copy, strum::Display)]
#[allow(missing_docs)]
pub enum ContainerState {
    Empty,
    Created,
    Running,
    Paused,
    Restarting,
    Removing,
    Exited,
    Dead,
}

impl From<bollard::secret::ContainerStateStatusEnum> for ContainerState {
    fn from(value: bollard::secret::ContainerStateStatusEnum) -> Self {
        match value {
            bollard::secret::ContainerStateStatusEnum::EMPTY => ContainerState::Empty,
            bollard::secret::ContainerStateStatusEnum::CREATED => ContainerState::Created,
            bollard::secret::ContainerStateStatusEnum::RUNNING => ContainerState::Running,
            bollard::secret::ContainerStateStatusEnum::PAUSED => ContainerState::Paused,
            bollard::secret::ContainerStateStatusEnum::RESTARTING => ContainerState::Restarting,
            bollard::secret::ContainerStateStatusEnum::REMOVING => ContainerState::Removing,
            bollard::secret::ContainerStateStatusEnum::EXITED => ContainerState::Exited,
            bollard::secret::ContainerStateStatusEnum::DEAD => ContainerState::Dead,
        }
    }
}

#[derive(Default)]
pub struct ContainerLogSource {
    pub log_stderr: bool,
    pub log_stdout: bool,
    pub follow: bool,
}

impl From<MessageSource> for ContainerLogSource {
    fn from(value: MessageSource) -> Self {
        match value {
            MessageSource::Stdout => ContainerLogSource {
                log_stdout: true,
                ..Default::default()
            },
            MessageSource::Stderr => ContainerLogSource {
                log_stderr: true,
                ..Default::default()
            },
        }
    }
}

pub struct LogEntry {
    pub message: Bytes,
    pub source: MessageSource,
}

impl Docker {
    pub async fn unpause(&self, container_name: &str) -> Result<(), DockerTestError> {
        self.client
            .unpause_container(container_name)
            .await
            .map_err(|e| DockerTestError::Daemon(format!("failed to unpause container: {e:?}")))
    }
    pub async fn kill(&self, container_name: &str) -> Result<(), DockerTestError> {
        self.client
            .kill_container(
                container_name,
                Some(KillContainerOptions { signal: "SIGKILL" }),
            )
            .await
            .map_err(|e| DockerTestError::Daemon(format!("failed to kill container: {e:?}")))
    }
    pub async fn pause(&self, container_name: &str) -> Result<(), DockerTestError> {
        self.client
            .pause_container(container_name)
            .await
            .map_err(|e| DockerTestError::Daemon(format!("failed to pause container: {e:?}")))
    }
    pub async fn container_state(
        &self,
        container_name: &str,
    ) -> Result<ContainerState, DockerTestError> {
        self.client
            .inspect_container(container_name, None::<InspectContainerOptions>)
            .await
            .map_err(|e| {
                DockerTestError::Daemon(format!("failed to check container state: {e:?}"))
            })?
            .state
            .ok_or(DockerTestError::Daemon(
                "container state was 'None'".to_string(),
            ))?
            .status
            .ok_or(DockerTestError::Daemon(
                "container status was 'None'".to_string(),
            ))
            .map(|v| v.into())
    }

    pub fn container_logs(
        &self,
        container_id: &str,
        source: ContainerLogSource,
    ) -> impl Stream<Item = Result<LogEntry, DockerTestError>> {
        let mut log_options = bollard::container::LogsOptions::<String> {
            follow: true,
            ..Default::default()
        };

        if source.log_stderr {
            log_options.stderr = true;
        }

        if source.log_stdout {
            log_options.stdout = true;
        }

        self.client
            .logs(container_id, Some(log_options))
            .filter_map(move |chunk| {
                let out = match chunk {
                    Ok(chunk) => match chunk {
                        LogOutput::StdErr { message } if source.log_stderr => Some(Ok(LogEntry {
                            message,
                            source: MessageSource::Stderr,
                        })),
                        LogOutput::StdOut { message } if source.log_stdout => Some(Ok(LogEntry {
                            message,
                            source: MessageSource::Stdout,
                        })),
                        _ => None,
                    },
                    Err(e) => Some(Err(DockerTestError::ContainerLogStream(e.to_string()))),
                };

                futures::future::ready(out)
            })
    }

    pub async fn get_container_ip_and_ports(
        &self,
        container_id: &str,
        network_name: &str,
    ) -> Result<ContainerInfo, DockerTestError> {
        let details = self
            .client
            .inspect_container(container_id, None::<InspectContainerOptions>)
            .await
            .map_err(|e| DockerTestError::Daemon(format!("failed to inspect container: {}", e)))?;

        // Get the ip address from the network
        let container_ip = if let Some(inspected_network) = details
            .network_settings
            .as_ref()
            .unwrap()
            .networks
            .as_ref()
            .unwrap()
            .get(network_name)
        {
            event!(
                Level::DEBUG,
                "container ip from inspect: {}",
                inspected_network.ip_address.as_ref().unwrap()
            );
            inspected_network
                .ip_address
                .as_ref()
                .unwrap()
                .parse::<std::net::Ipv4Addr>()
                // Exited containers will not have an IP address
                .unwrap_or_else(|e| {
                    event!(Level::TRACE, "container ip address failed to parse: {}", e);
                    std::net::Ipv4Addr::UNSPECIFIED
                })
        } else {
            std::net::Ipv4Addr::UNSPECIFIED
        };

        let host_ports = if let Some(ports) = details.network_settings.unwrap().ports {
            event!(
                Level::DEBUG,
                "container ports from inspect: {:?}",
                ports.clone()
            );
            HostPortMappings::try_from(ports)
                .map_err(|e| DockerTestError::HostPort(e.to_string()))?
        } else {
            HostPortMappings::default()
        };

        Ok(ContainerInfo {
            ip: container_ip,
            ports: host_ports,
        })
    }

    pub async fn start_container(
        &self,
        pending: PendingContainer,
    ) -> Result<OperationalContainer, DockerTestError> {
        if pending.is_static {
            STATIC_CONTAINERS.start(&pending).await
        } else {
            self.start_container_inner(pending).await
        }
    }

    // Internal start method should only be invoked from the static mod.
    // TODO: isolate to static mod only
    pub async fn start_container_inner(
        &self,
        mut pending: PendingContainer,
    ) -> Result<OperationalContainer, DockerTestError> {
        self.client
            .start_container(&pending.name, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| match e {
                Error::DockerResponseServerError {
                    message,
                    status_code,
                } => {
                    if status_code == 404 {
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
                    } else {
                        DockerTestError::Daemon(format!("failed to start container: {}", message))
                    }
                }
                _ => DockerTestError::Daemon(format!("failed to start container: {}", e)),
            })?;

        pending.wait.take().unwrap().wait_for_ready(pending).await
    }

    pub async fn stop_containers(&self, containers: &[CleanupContainer]) {
        join_all(
            containers
                .iter()
                .filter(|c| !c.is_static())
                .map(|c| {
                    self.client
                        .stop_container(&c.id, None::<StopContainerOptions>)
                })
                .collect::<Vec<_>>(),
        )
        .await;
    }

    pub async fn remove_containers(&self, containers: &[CleanupContainer]) {
        let futures = containers
            .iter()
            .filter(|c| !c.is_static())
            .map(|c| {
                // It's unlikely that anonymous volumes will be used by several containers.
                // In this case there will be remove errors that it's possible just to ignore
                // See:
                // https://github.com/moby/moby/blob/7b9275c0da707b030e62c96b679a976f31f929d3/daemon/mounts.go#L34).
                let options = Some(RemoveContainerOptions {
                    force: true,
                    v: true,
                    ..Default::default()
                });

                self.client.remove_container(&c.id, options)
            })
            .collect::<Vec<_>>();
        join_all(futures).await;
    }

    // TODO: isolate to static mod only
    pub async fn remove_container(&self, id: &str) {
        let remove_opts = Some(RemoveContainerOptions {
            force: true,
            ..Default::default()
        });

        if let Err(e) = self.client.remove_container(id, remove_opts).await {
            event!(Level::ERROR, "failed to remove static container: {}", e);
        }
    }

    // TODO: isolate to static mod only
    pub async fn disconnect_container(&self, container_id: &str, network: &str) {
        let opts = DisconnectNetworkOptions::<&str> {
            container: container_id,
            force: true,
        };
        if let Err(e) = self.client.disconnect_network(network, opts).await {
            event!(
                Level::ERROR,
                "unable to remove dockertest-container from network: {}",
                e
            );
        }
    }

    // TODO: isolate to static mod only
    pub async fn running_container_from_composition(
        &self,
        composition: Composition,
        container_details: ContainerInspectResponse,
    ) -> Result<OperationalContainer, DockerTestError> {
        if let Some(id) = container_details.id {
            Ok(OperationalContainer {
                client: self.clone(),
                id,
                name: composition.container_name.clone(),
                handle: composition.container_name,
                ip: std::net::Ipv4Addr::UNSPECIFIED,
                ports: HostPortMappings::default(),
                is_static: true,
                log_options: composition.log_options,
                assumed_state: Arc::new(Mutex::new(composition.wait.expected_state())),
            })
        } else {
            Err(DockerTestError::Daemon(
                "failed to retrieve container id for external container".to_string(),
            ))
        }
    }
}
