//! Implemenet static container abstraction.

use crate::{
    container::{CreatedContainer, HostPortMappings, StaticExternalContainer},
    Composition, DockerTestError, PendingContainer, RunningContainer, StaticManagementPolicy,
};

use bollard::{
    container::{InspectContainerOptions, RemoveContainerOptions},
    models::{ContainerInspectResponse, ContainerStateStatusEnum},
    network::DisconnectNetworkOptions,
    Docker,
};
use lazy_static::lazy_static;
use tokio::sync::RwLock;
use tracing::{event, Level};

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
    internal: Arc<RwLock<HashMap<String, InternalContainer>>>,
    external: Arc<RwLock<HashMap<String, RunningContainer>>>,
    dynamic: Arc<RwLock<HashMap<String, DynamicStatus>>>,
}

#[derive(Clone)]
enum DynamicStatus {
    /// The container was running prior to test invocation.
    /// For all these containers we essentially handle them the way we handle external containers.
    RunningPrior(RunningContainer),
    /// The container is in a running state and was not running prior to test invocation
    Running(RunningContainer, PendingContainer),
    Pending(PendingContainer),
    Failed(DockerTestError, Option<String>),
}

/// Represent a single internal, managed container.
struct InternalContainer {
    /// Current status of the container.
    status: InternalStatus,

    /// Represents a usage counter of the internal container.
    ///
    /// This counter is incremented for each test that uses this internal container.
    /// On test completion each test will decrement this counter and test which decrements it to 0
    /// will perform the cleanup of the container.
    completion_counter: u8,
}

/// Represents the different states of a internal container.
// NOTE: allowing this clippy warning in pending of refactor
#[allow(clippy::large_enum_variant)]
enum InternalStatus {
    /// As tests execute concurrently other tests might have already executed the WaitFor
    /// implementation and created a running container. However, as we do not want to alter our
    /// pipeline of Composition -> PendingContainer -> RunningContainer and start order logistics.
    /// We store a clone of the pending container here such that tests can return a
    /// clone of it if they are "behind" in the pipeline.
    Running(RunningContainer, PendingContainer),
    Pending(PendingContainer),
    /// If a test utilizes the same managed internal container with other tests, and completes
    /// the entire test including cleanup prior to other tests even registering their need for
    /// the same managed internal container, then it will be cleaned up. This is to avoid
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

impl InternalStatus {
    fn container_id(&self) -> Option<&str> {
        match &self {
            InternalStatus::Running(_, r) => Some(r.id.as_str()),
            InternalStatus::Pending(p) => Some(p.id.as_str()),
            InternalStatus::Failed(_, container_id) => container_id.as_ref().map(|id| id.as_str()),
            InternalStatus::Cleaned => None,
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
    ) -> Result<CreatedContainer, DockerTestError> {
        if let Some(policy) = composition.static_management_policy() {
            match policy {
                StaticManagementPolicy::Internal => self
                    .create_internal_container(composition, client, network)
                    .await
                    .map(CreatedContainer::Pending),
                StaticManagementPolicy::External => {
                    let external = self
                        .include_external_container(
                            composition,
                            client,
                            // Do not include network for external container if the network
                            // is already an existing network, externally managed.
                            network.filter(|_| !is_external_network),
                        )
                        .await?;
                    Ok(CreatedContainer::StaticExternal(external))
                }
                StaticManagementPolicy::Dynamic => {
                    self.include_or_create_dynamic_container(composition, client, network)
                        .await
                }
            }
        } else {
            Err(DockerTestError::Startup(
                "tried to create static container without a management policy".to_string(),
            ))
        }
    }

    pub async fn external_containers(&self) -> Vec<RunningContainer> {
        let mut external: Vec<RunningContainer> = {
            let map = self.external.read().await;
            map.values().cloned().collect()
        };

        // OnDemand containers that were running prior to test invocation are managed the same way
        // as external containers
        let on_demand_map = self.dynamic.read().await;

        external.extend(
            on_demand_map
                .values()
                .filter_map(|d| match d {
                    DynamicStatus::Running(_, _)
                    | DynamicStatus::Pending(_)
                    | DynamicStatus::Failed(_, _) => None,
                    DynamicStatus::RunningPrior(c) => Some(c),
                })
                .cloned(),
        );
        external
    }

    async fn include_or_create_dynamic_container(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<CreatedContainer, DockerTestError> {
        let mut map = self.dynamic.write().await;

        if let Some(status) = map.get(&composition.container_name) {
            match status {
                DynamicStatus::Pending(p) | DynamicStatus::Running(_, p) => {
                    Ok(CreatedContainer::Pending(p.clone()))
                }
                DynamicStatus::Failed(e, _) => Err(e.clone()),
                DynamicStatus::RunningPrior(c) => {
                    Ok(CreatedContainer::StaticExternal(StaticExternalContainer {
                        handle: c.handle.clone(),
                    }))
                }
            }
        } else {
            let details = client
                .inspect_container(&composition.container_name, None::<InspectContainerOptions>)
                .await;

            match details {
                Ok(d) => {
                    if let Some(container_state) = &d.state {
                        if let Some(status) = container_state.status {
                            if status != ContainerStateStatusEnum::RUNNING {
                                let options = Some(RemoveContainerOptions {
                                    force: true,
                                    ..Default::default()
                                });
                                client
                                    .remove_container(&composition.container_name, options)
                                    .await
                                    .map_err(|e| {
                                        DockerTestError::Daemon(format!(
                                            "failed to remove existing container: {}",
                                            e
                                        ))
                                    })?;
                            }
                        }
                    }
                    let running = self
                        .running_container_from_composition(composition, client, d)
                        .await?;

                    let external = StaticExternalContainer {
                        handle: running.handle.clone(),
                    };
                    map.insert(running.name.clone(), DynamicStatus::RunningPrior(running));

                    Ok(CreatedContainer::StaticExternal(external))
                }
                Err(e) => match e {
                    bollard::errors::Error::DockerResponseNotFoundError { message: _ } => {
                        let pending = self
                            .create_dynamic_container(composition, client, network)
                            .await?;
                        map.insert(
                            pending.name.clone(),
                            DynamicStatus::Pending(pending.clone()),
                        );
                        Ok(CreatedContainer::Pending(pending))
                    }
                    _ => Err(DockerTestError::Daemon(format!(
                        "failed to inspect dynamic container: {}",
                        e
                    ))),
                },
            }
        }
    }

    async fn include_external_container(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<StaticExternalContainer, DockerTestError> {
        let mut map = self.external.write().await;

        if let Some(running) = map.get(&composition.container_name) {
            if let Some(n) = network {
                self.add_to_network(running.id(), n, client).await?;
            }
            let external = StaticExternalContainer {
                handle: running.handle.clone(),
            };
            Ok(external)
        } else {
            let details = client
                .inspect_container(&composition.container_name, None::<InspectContainerOptions>)
                .await
                .map_err(|e| {
                    DockerTestError::Daemon(format!("failed to inspect external container: {}", e))
                })?;

            let running = self
                .running_container_from_composition(composition, client, details)
                .await?;
            let external = StaticExternalContainer {
                handle: running.handle.clone(),
            };
            map.insert(running.name.clone(), running);

            Ok(external)
        }
    }

    async fn create_dynamic_container(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<PendingContainer, DockerTestError> {
        let pending = composition.create_inner(client, network).await?;

        if let Some(n) = network {
            self.add_to_network(&pending.id, n, client).await?;
        }

        Ok(pending)
    }

    async fn create_internal_container(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<PendingContainer, DockerTestError> {
        let container = self
            .create_internal_container_inner(composition, client, network)
            .await?;

        if let Some(n) = network {
            self.add_to_network(&container.id, n, client).await?;
        }

        Ok(container)
    }

    async fn create_internal_container_inner(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<PendingContainer, DockerTestError> {
        let mut map = self.internal.write().await;

        // If we are the first test to try to create this container we are responsible for
        // container creation and inserting a InternalContainer in the global map with the
        // PendingContainer instance.
        // Static containers have to be apart of all the test networks such that they
        // are reachable from all tests.
        if let Some(c) = map.get_mut(&composition.container_name) {
            match &c.status {
                InternalStatus::Pending(p) => {
                    c.completion_counter += 1;
                    Ok(p.clone())
                }
                InternalStatus::Failed(e, _) => {
                    c.completion_counter += 1;
                    Err(e.clone())
                }
                InternalStatus::Running(_, p) => {
                    c.completion_counter += 1;
                    Ok(p.clone())
                }
                InternalStatus::Cleaned => {
                    self.create_internal_container_impl(&mut map, composition, client, network)
                        .await
                }
            }
        } else {
            self.create_internal_container_impl(&mut map, composition, client, network)
                .await
        }
    }

    async fn create_internal_container_impl(
        &self,
        containers: &mut HashMap<String, InternalContainer>,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<PendingContainer, DockerTestError> {
        let container_name = composition.container_name.clone();
        let pending = composition.create_inner(client, network).await;
        match pending {
            Ok(p) => {
                let c = InternalContainer {
                    status: InternalStatus::Pending(p.clone()),
                    completion_counter: 1,
                };
                containers.insert(container_name, c);
                Ok(p)
            }
            Err(e) => {
                let c = InternalContainer {
                    status: InternalStatus::Failed(e.clone(), None),
                    completion_counter: 1,
                };
                containers.insert(container_name, c);
                Err(e)
            }
        }
    }

    /// Start the PendingContainer representing a internal/dynamic container.
    ///
    /// It is the responsibility of the test runner to use this interface to
    /// obtain a RunningContainer from a PendingContainer that is statically managed.
    ///
    /// This method is responsible for setting the container id field of the Failed enum variant
    /// incase it fails to start the container.
    pub async fn start(
        &self,
        container: &PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        if let Some(policy) = &container.static_management_policy {
            match policy {
                StaticManagementPolicy::External => Err(DockerTestError::Startup(
                    "tried to start an external container".to_string(),
                )),
                StaticManagementPolicy::Internal => self.start_internal_container(container).await,
                StaticManagementPolicy::Dynamic => self.start_dynamic_container(container).await,
            }
        } else {
            Err(DockerTestError::Startup(
                "tried to start a non-exiting static container".to_string(),
            ))
        }
    }

    async fn start_dynamic_container(
        &self,
        container: &PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        let mut map = self.dynamic.write().await;

        if let Some(status) = map.get_mut(&container.name) {
            match &status {
                DynamicStatus::Running(r, _) | DynamicStatus::RunningPrior(r) => Ok(r.clone()),
                DynamicStatus::Pending(p) => {
                    let cloned = p.clone();
                    let running = cloned.start_internal().await;
                    match running {
                        Ok(r) => {
                            *status = DynamicStatus::Running(r.clone(), p.clone());
                            Ok(r)
                        }
                        Err(e) => {
                            *status = DynamicStatus::Failed(e.clone(), Some(container.id.clone()));
                            Err(e)
                        }
                    }
                }
                DynamicStatus::Failed(e, _) => Err(e.clone()),
            }
        } else {
            Err(DockerTestError::Startup(
                "tried to start a non-existing dynamic container".to_string(),
            ))
        }
    }

    async fn start_internal_container(
        &self,
        container: &PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        let mut map = self.internal.write().await;

        // If we are the first test to try to start the container we are responsible for starting
        // it and adding the RunningContainer instance to the global map.
        // We also keep the PendingContainer instance as other tests might not have reached this
        // far in their pipeline and need a PendingContainer instance.
        if let Some(c) = map.get_mut(&container.name) {
            match &c.status {
                InternalStatus::Failed(e, _) => Err(e.clone()),
                InternalStatus::Running(r, _) => Ok(r.clone()),
                InternalStatus::Pending(p) => {
                    let cloned = p.clone();
                    let running = cloned.start_internal().await;
                    match running {
                        Ok(r) => {
                            c.status = InternalStatus::Running(r.clone(), p.clone());
                            Ok(r)
                        }
                        Err(e) => {
                            c.status =
                                InternalStatus::Failed(e.clone(), Some(container.id.clone()));
                            Err(e)
                        }
                    }
                }
                // This should never occur as we set the completion_counter to 1 when
                // we encounter a Cleaned container during creation, hence the container will never
                // have it status set to Cleaned before this `Dockertest` instance has performed
                // cleanup.
                InternalStatus::Cleaned => Err(DockerTestError::Startup(
                    "encountered a cleaned container during startup".to_string(),
                )),
            }
        } else {
            Err(DockerTestError::Startup(
                "tried to start a non-existing internal container".to_string(),
            ))
        }
    }

    /// Disconnects all internal containers from the given network.
    ///
    /// If the network is external, we do not disconnect the external containers.
    async fn disconnect_internal_containers_from_network(
        &self,
        client: &Docker,
        network: &str,
        is_external_network: bool,
        to_cleanup: &HashSet<&str>,
    ) {
        self.disconnect_internal_containers(client, network, to_cleanup)
            .await;

        self.disconnect_dynamic_containers(client, network).await;

        // If we are operating with an existing network, we assume that this network
        // is externally managed for the external container.
        if !is_external_network {
            self.disconnect_external_containers(client, network, to_cleanup)
                .await;
        }
    }

    async fn disconnect_internal_containers(
        &self,
        client: &Docker,
        network: &str,
        to_cleanup: &HashSet<&str>,
    ) {
        let map = self.internal.read().await;
        for (_, container) in map.iter() {
            if let InternalStatus::Running(r, _) = &container.status {
                if to_cleanup.contains(r.id()) {
                    disconnect_container(client, r.id(), network).await;
                }
            }
        }
    }

    async fn disconnect_dynamic_containers(&self, client: &Docker, network: &str) {
        let map = self.dynamic.read().await;
        for (_, status) in map.iter() {
            match status {
                DynamicStatus::RunningPrior(r) | DynamicStatus::Running(r, _) => {
                    disconnect_container(client, r.id(), network).await;
                }
                DynamicStatus::Pending(_) | DynamicStatus::Failed(_, _) => (),
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
        self.disconnect_internal_containers_from_network(
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
        let mut containers = self.internal.write().await;

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
                        container.status = InternalStatus::Cleaned;
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

    async fn running_container_from_composition(
        &self,
        composition: Composition,
        client: &Docker,
        container_details: ContainerInspectResponse,
    ) -> Result<RunningContainer, DockerTestError> {
        if let Some(id) = container_details.id {
            Ok(RunningContainer {
                client: client.clone(),
                id,
                name: composition.container_name.clone(),
                handle: composition.container_name,
                ip: std::net::Ipv4Addr::UNSPECIFIED,
                ports: HostPortMappings::default(),
                is_static: true,
                log_options: composition.log_options,
            })
        } else {
            Err(DockerTestError::Daemon(
                "failed to retrieve container id for external container".to_string(),
            ))
        }
    }
}

impl Default for StaticContainers {
    fn default() -> StaticContainers {
        StaticContainers {
            internal: Arc::new(RwLock::new(HashMap::new())),
            external: Arc::new(RwLock::new(HashMap::new())),
            dynamic: Arc::new(RwLock::new(HashMap::new())),
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
