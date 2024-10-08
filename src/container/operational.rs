//! Represents a container that has been started, completing its WaitFor condition.

use crate::{
    composition::LogOptions,
    container::PendingContainer,
    docker::{ContainerState, Docker},
    waitfor::{wait_for_message, MessageSource},
    DockerTestError,
};

use bollard::models::{PortBinding, PortMap};
use serde::Serialize;

use std::{
    collections::HashMap,
    convert::TryFrom,
    net::{IpAddr, Ipv4Addr},
    str::FromStr,
    sync::{Arc, Mutex},
};

/// Represent a docker container that has passed its `WaitFor` implementation and available is to the test body.
/// The state of the container will depend on what `WaitFor` implementation was used, e.g.
/// using the `ExitedWait` will result in a container in an exited state.
// NOTE: Fields within this structure are pub(crate) only for testability.
// None of these fields should be externally public.
#[derive(Clone, Debug)]
pub struct OperationalContainer {
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
    pub(crate) assumed_state: Arc<Mutex<ContainerState>>,
}

#[derive(Clone, Debug, Default)]
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

impl OperationalContainer {
    /// Return the generated name on the docker container object for this `OperationalContainer`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the docker assigned identifier for this `OperationalContainer`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Return the IPv4 address for this container on the local docker network adapter.
    /// Use this address to contact the `OperationalContainer` in the test body.
    ///
    /// This property is retrieved from the docker daemon prior to entering the test body.
    /// It is cached internally and not updated between invocations. This means that
    /// if the docker container enters an exited state, this function will still return
    /// the original ip assigned to the container.
    ///
    /// If the [ExitedWait] for strategy is employed, the `OperationalContainer` will
    /// be in an exited status when the test body is entered.
    /// For this scenarion, this function will return [Ipv4Addr::UNSPECIFIED].
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

    /// Pauses the container
    ///
    /// # Panics
    /// Will panic if the container was in any state but `Runnning` prior to calling this
    /// method or if we fail the docker operation.
    pub async fn pause(&self) {
        self.goto_state(ContainerState::Paused, &[ContainerState::Running])
            .unwrap();
        self.client.pause(&self.name).await.unwrap();
    }

    /// Kills the container
    ///
    /// # Panics
    /// Will panic if the container was in any state but `Runnning` or `Paused` prior to calling this
    /// method or if we fail the docker operation.
    pub async fn kill(&self) {
        self.goto_state(
            ContainerState::Exited,
            &[ContainerState::Running, ContainerState::Paused],
        )
        .unwrap();
        self.client.kill(&self.name).await.unwrap();
    }

    /// Unpauses the container
    ///
    /// # Panics
    /// Will panic if the container was in any state but `Paused` prior to calling this
    /// method or if we fail the docker operation.
    pub async fn unpause(&self) {
        self.goto_state(ContainerState::Running, &[ContainerState::Paused])
            .unwrap();
        self.client.unpause(&self.name).await.unwrap();
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

    fn goto_state(
        &self,
        dest: ContainerState,
        allowed: &[ContainerState],
    ) -> Result<(), DockerTestError> {
        let mut state = self.assumed_state.lock().unwrap();
        println!("{state}");
        if !allowed.contains(&state) {
            Err(DockerTestError::ContainerState {
                current: *state,
                tried_to_enter: dest,
            })
        } else {
            *state = dest;
            Ok(())
        }
    }
}

impl From<PendingContainer> for OperationalContainer {
    fn from(container: PendingContainer) -> OperationalContainer {
        OperationalContainer {
            client: container.client,
            handle: container.handle,
            id: container.id,
            name: container.name,
            ip: std::net::Ipv4Addr::UNSPECIFIED,
            ports: HostPortMappings::default(),
            is_static: container.is_static,
            log_options: container.log_options,
            assumed_state: Arc::new(Mutex::new(container.expected_state)),
        }
    }
}
