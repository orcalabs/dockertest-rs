use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::RwLock;

use bollard::Docker;

use super::{add_to_network, disconnect_container, remove_container};
use crate::{
    composition::Composition, DockerTestError, Network, PendingContainer, RunningContainer,
};

#[derive(Default)]
pub struct InternalContainers {
    inner: Arc<RwLock<HashMap<String, InternalContainer>>>,
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

impl InternalContainers {
    pub async fn create(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
        network_setting: &Network,
    ) -> Result<PendingContainer, DockerTestError> {
        let container = self
            .create_internal_container_inner(composition, client, network, network_setting)
            .await?;

        Ok(container)
    }

    pub async fn start(
        &self,
        container: &PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        let mut map = self.inner.write().await;

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

    pub async fn cleanup(&self, client: &Docker, network: &str, to_cleanup: &HashSet<&str>) {
        self.disconnect(client, network, to_cleanup).await;
        let to_remove = self.decrement_completion_counters(to_cleanup).await;
        for to_cleanup in to_remove {
            remove_container(&to_cleanup, client).await;
        }
    }

    async fn create_internal_container_inner(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
        network_setting: &Network,
    ) -> Result<PendingContainer, DockerTestError> {
        let mut map = self.inner.write().await;

        // If we are the first test to try to create this container we are responsible for
        // container creation and inserting a InternalContainer in the global map with the
        // PendingContainer instance.
        // Static containers have to be apart of all the test networks such that they
        // are reachable from all tests.
        //
        // When `Network::Singular/Network::External` is used only the first test needs to add it to the
        // network.
        if let Some(c) = map.get_mut(&composition.container_name) {
            match &c.status {
                InternalStatus::Pending(p) | InternalStatus::Running(_, p) => {
                    // Only when the Isolated network mode is set do we need to add it to the
                    // network, as for External/Singular it will be added upon creation.
                    match (network, network_setting) {
                        (Some(n), Network::Isolated) => add_to_network(&p.id, n, client).await,
                        _ => Ok(()),
                    }?;

                    c.completion_counter += 1;

                    Ok(p.clone())
                }
                InternalStatus::Failed(e, _) => {
                    c.completion_counter += 1;
                    Err(e.clone())
                }
                InternalStatus::Cleaned => {
                    let container = self
                        .create_internal_container_impl(&mut map, composition, client, network)
                        .await?;

                    // This is the same case as upon first container creation
                    if let Some(n) = network {
                        add_to_network(&container.id, n, client).await?;
                    }

                    Ok(container)
                }
            }
        } else {
            let container = self
                .create_internal_container_impl(&mut map, composition, client, network)
                .await?;

            // First to create the container adds it to the network regardless of which network
            // mode is set
            if let Some(n) = network {
                add_to_network(&container.id, n, client).await?;
            }

            Ok(container)
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

    async fn disconnect(&self, client: &Docker, network: &str, to_cleanup: &HashSet<&str>) {
        let map = self.inner.read().await;
        for (_, container) in map.iter() {
            if let InternalStatus::Running(r, _) = &container.status {
                if to_cleanup.contains(r.id()) {
                    disconnect_container(client, r.id(), network).await;
                }
            }
        }
    }

    async fn decrement_completion_counters(&self, to_cleanup: &HashSet<&str>) -> Vec<String> {
        let mut containers = self.inner.write().await;

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
