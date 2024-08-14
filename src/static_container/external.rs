use crate::{
    composition::Composition, container::StaticExternalContainer, docker::Docker, DockerTestError,
    Network, RunningContainer,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::RwLock;

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
                        client.add_container_to_network(running.id(), n).await?;
                    }
                }
            }
            let external = StaticExternalContainer {
                handle: running.handle.clone(),
                id: running.id().to_string(),
            };
            Ok(external)
        } else {
            let running = client
                .include_static_external_container(composition, network, network_mode)
                .await?;

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
                client.disconnect_container(container.id(), network).await;
            }
        }
    }
}
