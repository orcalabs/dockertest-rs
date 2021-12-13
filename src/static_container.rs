use crate::{
    container::HostPortMappings, Composition, DockerTestError, PendingContainer, RunningContainer,
    StaticManagementPolicy,
};
use bollard::{
    container::{InspectContainerOptions, RemoveContainerOptions},
    network::DisconnectNetworkOptions,
    Docker,
};
use lazy_static::lazy_static;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{event, Level};

// Internal static object to keep track of all static containers.
//
// Each test binary will create one of these static container objects, and we rely on the fact that
// only one test binary is executed at a time.
// Within a test binary multiple might execute in parallel, but only a single test binary is
// executed at a time (https://github.com/rust-lang/cargo/issues/5609).
// If the issue is resolved in the future we can change our implementation accordingly.
lazy_static! {
    pub(crate) static ref STATIC_CONTAINERS: StaticContainers = StaticContainers::default();
}

/// Encapsulates all static container related logic.
///
/// Synchronizes the creation and starting of static containers.
/// Callers are still responsible for checking if the container they are starting/creating are
/// static and call the appropriate method.
pub struct StaticContainers {
    containers: Arc<RwLock<HashMap<String, StaticContainer>>>,
    external: Arc<RwLock<HashMap<String, RunningContainer>>>,
}

/// Represent a single static, managed container.
struct StaticContainer {
    /// Current status of the container.
    status: StaticStatus,

    /// Represents a usage counter of the static container.
    ///
    /// This counter is incremented for each test that uses this static container.
    /// On test completion each test will decrement this counter and test which decrements it to 0
    /// will perform the cleanup of the container.
    completion_counter: u8,
}

/// Represents the different states of a static container.
// NOTE: allowing this clippy warning in pending of refactor
#[allow(clippy::large_enum_variant)]
enum StaticStatus {
    /// As tests execute concurrently other tests might have already executed the WaitFor
    /// implementation and created a running container. However, as we do not want to alter our
    /// pipeline of Composition -> PendingContainer -> RunningContainer and start order logistics.
    /// We store a clone of the pending container here such that tests can return a
    /// clone of it if they are "behind" in the pipeline.
    Running(RunningContainer, PendingContainer),
    Pending(PendingContainer),
    /// If a test utilizes the same managed static container with other tests, and completes
    /// the entire test including cleanup prior to other tests even registering their need for
    /// the same managed static container, then it will be cleaned up. This is to avoid
    /// leaking the container on binary termination.
    ///
    /// The remaining tests should create the container again during container creation.
    /// This status is only set during container cleanup.
    Cleaned,
    /// Keeps the id of the failed container for cleanup purposes.
    ///
    /// If the container failed to be created the id will be None as we have not container id yet
    /// and no cleanup will be necessary.
    /// If the container failed to be started, the id will be present and we will need
    /// to remove the container.
    Failed(DockerTestError, Option<String>),
}

impl StaticStatus {
    fn container_id(&self) -> Option<&str> {
        match &self {
            StaticStatus::Running(_, r) => Some(r.id.as_str()),
            StaticStatus::Pending(p) => Some(p.id.as_str()),
            StaticStatus::Failed(_, container_id) => container_id.as_ref().map(|id| id.as_str()),
            StaticStatus::Cleaned => None,
        }
    }
}

impl StaticContainers {
    /// It is the responsibility of the Composition to invoke this method for creating
    /// a static version of it, in order to obtain a PendingContainer.
    pub async fn create(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
        is_external_network: bool,
    ) -> Result<Option<PendingContainer>, DockerTestError> {
        if let Some(policy) = composition.static_management_policy() {
            match policy {
                StaticManagementPolicy::DockerTest => self
                    .create_static_container(composition, client, network)
                    .await
                    .map(Some),
                StaticManagementPolicy::External => {
                    self.include_external_container(
                        composition,
                        client,
                        // Do not include network for external container if the network
                        // is already an existing network, externally managed.
                        network.filter(|_| !is_external_network),
                    )
                    .await?;
                    Ok(None)
                }
            }
        } else {
            Err(DockerTestError::Startup(
                "tried to create static container without a management policy".to_string(),
            ))
        }
    }

    pub async fn external_containers(&self) -> Vec<RunningContainer> {
        let map = self.external.read().await;

        map.values().cloned().collect()
    }

    async fn include_external_container(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<(), DockerTestError> {
        let mut map = self.external.write().await;

        if let Some(running) = map.get(&composition.container_name) {
            if let Some(n) = network {
                self.add_to_network(running.id(), n, client).await?;
            }
        } else {
            let details = client
                .inspect_container(&composition.container_name, None::<InspectContainerOptions>)
                .await
                .map_err(|e| {
                    DockerTestError::Daemon(format!("failed to inspect external container: {}", e))
                })?;

            if let Some(id) = details.id {
                if let Some(n) = network {
                    self.add_to_network(&id, n, client).await?;
                }
                let running = RunningContainer {
                    client: client.clone(),
                    id,
                    name: composition.container_name.clone(),
                    handle: composition.container_name.clone(),
                    ip: std::net::Ipv4Addr::UNSPECIFIED,
                    ports: HostPortMappings::default(),
                    is_static: true,
                    log_options: composition.log_options.clone(),
                };
                map.insert(composition.container_name, running);
            } else {
                return Err(DockerTestError::Daemon(
                    "failed to retrieve container id for external container".to_string(),
                ));
            }
        }

        Ok(())
    }

    async fn create_static_container(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<PendingContainer, DockerTestError> {
        let container = self
            .create_static_container_inner(composition, client, network)
            .await?;

        if let Some(n) = network {
            self.add_to_network(&container.id, n, client).await?;
        }

        Ok(container)
    }

    async fn create_static_container_inner(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<PendingContainer, DockerTestError> {
        let mut map = self.containers.write().await;

        // If we are the first test to try to create this container we are responsible for
        // container creation and inserting a StaticContainer in the global map with the
        // PendingContainer instance.
        // Static containers have to be apart of all the test networks such that they
        // are reachable from all tests.
        if let Some(c) = map.get_mut(&composition.container_name) {
            match &c.status {
                StaticStatus::Pending(p) => {
                    c.completion_counter += 1;
                    Ok(p.clone())
                }
                StaticStatus::Failed(e, _) => {
                    c.completion_counter += 1;
                    Err(e.clone())
                }
                StaticStatus::Running(_, p) => {
                    c.completion_counter += 1;
                    Ok(p.clone())
                }
                StaticStatus::Cleaned => {
                    self.create_static_container_impl(&mut map, composition, client, network)
                        .await
                }
            }
        } else {
            self.create_static_container_impl(&mut map, composition, client, network)
                .await
        }
    }

    async fn create_static_container_impl(
        &self,
        containers: &mut HashMap<String, StaticContainer>,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<PendingContainer, DockerTestError> {
        let container_name = composition.container_name.clone();
        let pending = composition.create_inner(client, network).await;
        match pending {
            Ok(p) => {
                let c = StaticContainer {
                    status: StaticStatus::Pending(p.clone()),
                    completion_counter: 1,
                };
                containers.insert(container_name, c);
                Ok(p)
            }
            Err(e) => {
                let c = StaticContainer {
                    status: StaticStatus::Failed(e.clone(), None),
                    completion_counter: 1,
                };
                containers.insert(container_name, c);
                Err(e)
            }
        }
    }

    /// Start the PendingContainer representing a static container.
    ///
    /// It is the responsibility of the test runner to use this interface to
    /// obtain a static a RunningContainer from a PendingContainer that is a managed.
    ///
    /// This method is responsible for setting the container id field of the Failed enum variant
    /// incase it fails to start the container.
    pub async fn start(
        &self,
        container: &PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        let mut map = self.containers.write().await;

        // If we are the first test to try to start the container we are responsible for starting
        // it and adding the RunningContainer instance to the global map.
        // We also keep the PendingContainer instance as other tests might not have reached this
        // far in their pipeline and need a PendingContainer instance.
        if let Some(c) = map.get_mut(&container.name) {
            match &c.status {
                StaticStatus::Failed(e, _) => Err(e.clone()),
                StaticStatus::Running(r, _) => Ok(r.clone()),
                StaticStatus::Pending(p) => {
                    let cloned = p.clone();
                    let running = cloned.start_internal().await;
                    match running {
                        Ok(r) => {
                            c.status = StaticStatus::Running(r.clone(), p.clone());
                            Ok(r)
                        }
                        Err(e) => {
                            c.status = StaticStatus::Failed(e.clone(), Some(container.id.clone()));
                            Err(e)
                        }
                    }
                }
                // This should never occur as we set the completion_counter to 1 when
                // we encounter a Cleaned container during creation, hence the container will never
                // have it status set to Cleaned before this `Dockertest` instance has performed
                // cleanup.
                StaticStatus::Cleaned => Err(DockerTestError::Startup(
                    "encountered a cleaned container during startup".to_string(),
                )),
            }
        } else {
            Err(DockerTestError::Startup(
                "tried to start a non-exiting static container".to_string(),
            ))
        }
    }

    /// Disconnects all static containers from the given network.
    ///
    /// If the network is external, we do not disconnect the external containers.
    async fn disconnect_static_containers_from_network(
        &self,
        client: &Docker,
        network: &str,
        is_external_network: bool,
        to_cleanup: &HashSet<&str>,
    ) {
        self.disconnect_static_containers(client, network, to_cleanup)
            .await;

        // If we are operating with an existing network, we assume that this network
        // is externally managed for the external container.
        if !is_external_network {
            self.disconnect_external_containers(client, network, to_cleanup)
                .await;
        }
    }

    async fn disconnect_static_containers(
        &self,
        client: &Docker,
        network: &str,
        to_cleanup: &HashSet<&str>,
    ) {
        let map = self.containers.read().await;
        for (_, container) in map.iter() {
            if let StaticStatus::Running(r, _) = &container.status {
                if to_cleanup.contains(r.id()) {
                    disconnect_container(client, r.id(), network).await;
                }
            }
        }
    }

    async fn disconnect_external_containers(
        &self,
        client: &Docker,
        network: &str,
        to_cleanup: &HashSet<&str>,
    ) {
        let map = self.external.read().await;
        for (_, container) in map.iter() {
            if to_cleanup.contains(container.id()) {
                disconnect_container(client, container.id(), network).await;
            }
        }
    }

    // We assume that all tests that usees a static container MUST invoke this method as apart of
    // dockertest teardown.
    // This is required to ensure static container cleanup is performed.
    pub async fn cleanup(
        &self,
        client: &Docker,
        network: &str,
        is_external_network: bool,
        to_cleanup: Vec<&str>,
    ) {
        let cleanup_map: HashSet<&str> = to_cleanup.into_iter().collect();
        // We have to remove any static containers from the network such that the network itself
        // can be deleted.
        // We do not need to hold the lock for network disconnection as each Dockertest instance
        // generates its own docker network.
        self.disconnect_static_containers_from_network(
            client,
            network,
            is_external_network,
            &cleanup_map,
        )
        .await;

        let to_remove = self.decrement_completion_counters(&cleanup_map).await;
        for id in to_remove {
            self.remove_container(&id, client).await;
        }
    }

    async fn decrement_completion_counters(&self, to_cleanup: &HashSet<&str>) -> Vec<String> {
        let mut containers = self.containers.write().await;

        let mut responsible_to_remove = Vec::new();

        // We assume that if the container failed to be started the container id will be
        // present on the Failure enum variant.
        // This should be set by the start method.
        for (_, container) in containers.iter_mut() {
            if let Some(container_id) = container.status.container_id() {
                if to_cleanup.contains(container_id) {
                    container.completion_counter -= 1;
                    if container.completion_counter == 0 {
                        responsible_to_remove.push(container_id.to_string());
                        container.status = StaticStatus::Cleaned;
                    }
                }
            }
        }
        responsible_to_remove
    }

    async fn remove_container(&self, id: &str, client: &Docker) {
        let remove_opts = Some(RemoveContainerOptions {
            force: true,
            ..Default::default()
        });

        if let Err(e) = client.remove_container(id, remove_opts).await {
            event!(Level::ERROR, "failed to remove static container: {}", e);
        }
    }

    async fn add_to_network(
        &self,
        container_id: &str,
        network: &str,
        client: &Docker,
    ) -> Result<(), DockerTestError> {
        let opts = bollard::network::ConnectNetworkOptions {
            container: container_id,
            endpoint_config: bollard::models::EndpointSettings::default(),
        };

        event!(
            Level::DEBUG,
            "adding to network: {}, container: {}",
            network,
            container_id
        );

        client.connect_network(network, opts).await.map_err(|e| {
            DockerTestError::Startup(format!(
                "failed to add static container to dockertest network: {}",
                e
            ))
        })
    }
}

impl Default for StaticContainers {
    fn default() -> StaticContainers {
        StaticContainers {
            containers: Arc::new(RwLock::new(HashMap::new())),
            external: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

async fn disconnect_container(client: &Docker, container_id: &str, network: &str) {
    let opts = DisconnectNetworkOptions::<&str> {
        container: container_id,
        force: true,
    };
    if let Err(e) = client.disconnect_network(network, opts).await {
        event!(
            Level::ERROR,
            "unable to remove dockertest-container from network: {}",
            e
        );
    }
}
