use super::{add_to_network, disconnect_container, running_container_from_composition};
use crate::{
    container::{CreatedContainer, StaticExternalContainer},
    Composition, DockerTestError, PendingContainer, RunningContainer,
};
use bollard::{
    container::{InspectContainerOptions, RemoveContainerOptions},
    models::ContainerStateStatusEnum,
    Docker,
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

#[derive(Default)]
pub struct DynamicContainers {
    inner: Arc<RwLock<HashMap<String, DynamicStatus>>>,
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

impl DynamicContainers {
    pub async fn create(
        &self,
        composition: Composition,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<CreatedContainer, DockerTestError> {
        let mut map = self.inner.write().await;

        if let Some(status) = map.get(&composition.container_name) {
            match status {
                DynamicStatus::Pending(p) | DynamicStatus::Running(_, p) => {
                    if let Some(n) = network {
                        add_to_network(&p.id, n, client).await?;
                    }
                    Ok(CreatedContainer::Pending(p.clone()))
                }
                DynamicStatus::Failed(e, _) => Err(e.clone()),
                DynamicStatus::RunningPrior(c) => {
                    if let Some(n) = network {
                        add_to_network(&c.id, n, client).await?;
                    }

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

                    if let Some(n) = network {
                        add_to_network(&running.id, n, client).await?;
                    }

                    let external = StaticExternalContainer {
                        handle: running.handle.clone(),
                        id: running.id().to_string(),
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

    pub async fn start(
        &self,
        container: &PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        let mut map = self.inner.write().await;

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

    pub async fn prior_running_containers(&self) -> Vec<RunningContainer> {
        self.inner
            .read()
            .await
            .values()
            .filter_map(|d| match d {
                DynamicStatus::Running(_, _)
                | DynamicStatus::Pending(_)
                | DynamicStatus::Failed(_, _) => None,
                DynamicStatus::RunningPrior(c) => Some(c),
            })
            .cloned()
            .collect()
    }

    pub async fn disconnect(&self, client: &Docker, network: &str) {
        let map = self.inner.read().await;
        for (_, status) in map.iter() {
            match status {
                DynamicStatus::RunningPrior(r) | DynamicStatus::Running(r, _) => {
                    disconnect_container(client, r.id(), network).await;
                }
                DynamicStatus::Pending(_) | DynamicStatus::Failed(_, _) => (),
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
