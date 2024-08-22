use crate::{
    composition::Composition,
    container::{CreatedContainer, StaticExternalContainer},
    docker::Docker,
    DockerTestError, Network, OperationalContainer, PendingContainer,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::RwLock;

#[derive(Default)]
pub struct DynamicContainers {
    inner: Arc<RwLock<HashMap<String, DynamicContainer>>>,
}

#[derive(Clone)]
pub struct DynamicContainer {
    status: DynamicStatus,
}

#[derive(Clone)]
pub enum CreateDynamicContainer {
    RunningPrior(OperationalContainer),
    Pending(PendingContainer),
}

#[derive(Clone)]
enum DynamicStatus {
    /// The container was running prior to test invocation.
    /// For all these containers we essentially handle them the way we handle external containers.
    RunningPrior(OperationalContainer),
    /// The container is in a running state and was not running prior to test invocation
    Running(OperationalContainer, PendingContainer),
    Pending(PendingContainer),
    Failed(DockerTestError),
}

impl DynamicContainers {
    pub async fn create(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
        network_setting: &Network,
    ) -> Result<CreatedContainer, DockerTestError> {
        let mut map = self.inner.write().await;

        if let Some(container) = map.get_mut(&composition.container_name) {
            match &container.status {
                DynamicStatus::Pending(p) | DynamicStatus::Running(_, p) => {
                    match (network, network_setting) {
                        (Some(n), Network::Isolated) => {
                            client.add_container_to_network(&p.id, n).await
                        }
                        _ => Ok(()),
                    }?;
                    Ok(CreatedContainer::Pending(p.clone()))
                }
                DynamicStatus::Failed(e) => Err(e.clone()),
                DynamicStatus::RunningPrior(c) => {
                    match (network, network_setting) {
                        (Some(n), Network::Isolated) => {
                            client.add_container_to_network(&c.id, n).await
                        }
                        _ => Ok(()),
                    }?;

                    Ok(CreatedContainer::StaticExternal(StaticExternalContainer {
                        handle: c.handle.clone(),
                        id: c.id().to_string(),
                    }))
                }
            }
        } else {
            match client
                .create_dynamic_container(composition, network)
                .await?
            {
                CreateDynamicContainer::RunningPrior(running) => {
                    let external = StaticExternalContainer {
                        handle: running.handle.clone(),
                        id: running.id().to_string(),
                    };

                    map.insert(
                        running.name.clone(),
                        DynamicContainer {
                            status: DynamicStatus::RunningPrior(running),
                        },
                    );

                    Ok(CreatedContainer::StaticExternal(external))
                }
                CreateDynamicContainer::Pending(pending) => {
                    map.insert(
                        pending.name.clone(),
                        DynamicContainer {
                            status: DynamicStatus::Pending(pending.clone()),
                        },
                    );
                    Ok(CreatedContainer::Pending(pending))
                }
            }
        }
    }

    pub async fn start(
        &self,
        container: &PendingContainer,
    ) -> Result<OperationalContainer, DockerTestError> {
        let mut map = self.inner.write().await;

        if let Some(existing) = map.get_mut(&container.name) {
            match &existing.status {
                DynamicStatus::Running(r, _) | DynamicStatus::RunningPrior(r) => Ok(r.clone()),
                DynamicStatus::Pending(p) => {
                    let cloned = p.clone();
                    let running = cloned.start_inner().await;
                    match running {
                        Ok(r) => {
                            existing.status = DynamicStatus::Running(r.clone(), p.clone());
                            Ok(r)
                        }
                        Err(e) => {
                            existing.status = DynamicStatus::Failed(e.clone());
                            Err(e)
                        }
                    }
                }
                DynamicStatus::Failed(e) => Err(e.clone()),
            }
        } else {
            Err(DockerTestError::Startup(
                "tried to start a non-existing dynamic container".to_string(),
            ))
        }
    }

    pub async fn prior_running_containers(&self) -> Vec<OperationalContainer> {
        self.inner
            .read()
            .await
            .values()
            .filter_map(|d| match &d.status {
                DynamicStatus::Running(_, _)
                | DynamicStatus::Pending(_)
                | DynamicStatus::Failed(_) => None,
                DynamicStatus::RunningPrior(c) => Some(c.clone()),
            })
            .collect()
    }

    pub async fn disconnect(
        &self,
        client: &Docker,
        network: &str,
        network_mode: &Network,
        to_cleanup: &HashSet<&str>,
    ) {
        match network_mode {
            Network::External(_) | Network::Singular => (),
            Network::Isolated => {
                let containers = self.inner.read().await;
                for (id, _) in containers.iter() {
                    if to_cleanup.contains(id.as_str()) {
                        client.disconnect_container(id, network).await;
                    }
                }
            }
        }
    }
}
