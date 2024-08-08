use super::{add_to_network, disconnect_container, running_container_from_composition};
use crate::{
    composition::Composition,
    container::{CreatedContainer, StaticExternalContainer},
    DockerTestError, Network, PendingContainer, RunningContainer,
};
use bollard::{
    container::{InspectContainerOptions, RemoveContainerOptions},
    models::ContainerStateStatusEnum,
    Docker,
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
enum DynamicStatus {
    /// The container was running prior to test invocation.
    /// For all these containers we essentially handle them the way we handle external containers.
    RunningPrior(RunningContainer),
    /// The container is in a running state and was not running prior to test invocation
    Running(RunningContainer, PendingContainer),
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
                        (Some(n), Network::Isolated) => add_to_network(&p.id, n, client).await,
                        _ => Ok(()),
                    }?;
                    Ok(CreatedContainer::Pending(p.clone()))
                }
                DynamicStatus::Failed(e) => Err(e.clone()),
                DynamicStatus::RunningPrior(c) => {
                    match (network, network_setting) {
                        (Some(n), Network::Isolated) => add_to_network(&c.id, n, client).await,
                        _ => Ok(()),
                    }?;

                    Ok(CreatedContainer::StaticExternal(StaticExternalContainer {
                        handle: c.handle.clone(),
                        id: c.id().to_string(),
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
                    let running =
                        running_container_from_composition(composition, client, d).await?;

                    // Regardless of network mode the first to create a Dynamic container is
                    // responsible for adding it to the network
                    if let Some(n) = network {
                        add_to_network(&running.id, n, client).await?;
                    }

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
                Err(e) => match e {
                    bollard::errors::Error::DockerResponseServerError {
                        message: _,
                        status_code,
                    } => {
                        // The container does not exists, we have to create it.
                        if status_code == 404 {
                            let pending = self
                                .create_dynamic_container(composition, client, network)
                                .await?;
                            map.insert(
                                pending.name.clone(),
                                DynamicContainer {
                                    status: DynamicStatus::Pending(pending.clone()),
                                },
                            );
                            Ok(CreatedContainer::Pending(pending))
                        } else {
                            Err(DockerTestError::Daemon(format!(
                                "failed to inspect dynamic container: {}",
                                e
                            )))
                        }
                    }
                    _ => Err(DockerTestError::Daemon(format!(
                        "failed to inspect dynamic container: {}",
                        e
                    ))),
                },
            }
        }
    }

    pub async fn start(
        &self,
        container: &PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        let mut map = self.inner.write().await;

        if let Some(existing) = map.get_mut(&container.name) {
            match &existing.status {
                DynamicStatus::Running(r, _) | DynamicStatus::RunningPrior(r) => Ok(r.clone()),
                DynamicStatus::Pending(p) => {
                    let cloned = p.clone();
                    let running = cloned.start_internal().await;
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

    pub async fn prior_running_containers(&self) -> Vec<RunningContainer> {
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
                        disconnect_container(client, id, network).await;
                    }
                }
            }
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
            add_to_network(&pending.id, n, client).await?;
        }

        Ok(pending)
    }
}
