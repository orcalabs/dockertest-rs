use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::RwLock;

use bollard::{container::InspectContainerOptions, Docker};

use super::{add_to_network, disconnect_container, running_container_from_composition};
use crate::{
    container::StaticExternalContainer, Composition, DockerTestError, Network, RunningContainer,
};

#[derive(Default)]
pub struct ExternalContainers {
    inner: Arc<RwLock<HashMap<String, RunningContainer>>>,
}

impl ExternalContainers {
    pub async fn include(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
        network_mode: &Network,
    ) -> Result<StaticExternalContainer, DockerTestError> {
        let mut map = self.inner.write().await;

        if let Some(running) = map.get(&composition.container_name) {
            match network_mode {
                Network::Singular | Network::External(_) => (),
                Network::Isolated => {
                    if let Some(n) = network {
                        add_to_network(running.id(), n, client).await?;
                    }
                }
            }
            let external = StaticExternalContainer {
                handle: running.handle.clone(),
                id: running.id().to_string(),
            };
            Ok(external)
        } else {
            let details = client
                .inspect_container(&composition.container_name, None::<InspectContainerOptions>)
                .await
                .map_err(|e| {
                    DockerTestError::Daemon(format!("failed to inspect external container: {}", e))
                })?;

            let running = running_container_from_composition(composition, client, details).await?;

            match network_mode {
                Network::External(_) => (),
                // The first to include external containers are responsible for including them in
                // the singular/isolated network
                Network::Isolated | Network::Singular => {
                    if let Some(n) = network {
                        add_to_network(running.id(), n, client).await?;
                    }
                }
            }

            let external = StaticExternalContainer {
                handle: running.handle.clone(),
                id: running.id().to_string(),
            };
            map.insert(running.name.clone(), running);

            Ok(external)
        }
    }

    pub async fn containers(&self) -> Vec<RunningContainer> {
        self.inner.read().await.values().cloned().collect()
    }

    pub async fn disconnect(
        &self,
        client: &Docker,
        network: &str,
        network_mode: &Network,
        to_cleanup: &HashSet<&str>,
    ) {
        // If we are operating with an existing network, we assume that this network
        // is externally managed for the external container.
        // For singular network we perform the same behavior, we do not disconnect.
        match network_mode {
            Network::Singular | Network::External(_) => (),
            Network::Isolated => {
                self.disconnect_impl(client, network, to_cleanup).await;
            }
        }
    }

    async fn disconnect_impl(&self, client: &Docker, network: &str, to_cleanup: &HashSet<&str>) {
        let map = self.inner.read().await;
        for (_, container) in map.iter() {
            if to_cleanup.contains(container.id()) {
                disconnect_container(client, container.id(), network).await;
            }
        }
    }
}
