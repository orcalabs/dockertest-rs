use crate::{
    composition::Composition, static_container::CreateDynamicContainer, DockerTestError, Network,
    OperationalContainer,
};
use bollard::{
    container::{InspectContainerOptions, RemoveContainerOptions},
    secret::ContainerStateStatusEnum,
};

use super::Docker;

impl Docker {
    pub async fn include_static_external_container(
        &self,
        composition: Composition,
        network: Option<&str>,
        network_mode: &Network,
    ) -> Result<OperationalContainer, DockerTestError> {
        let details = self
            .client
            .inspect_container(&composition.container_name, None::<InspectContainerOptions>)
            .await
            .map_err(|e| {
                DockerTestError::Daemon(format!("failed to inspect external container: {}", e))
            })?;

        let running = self
            .running_container_from_composition(composition, details)
            .await?;

        match network_mode {
            Network::External(_) => (),
            // The first to include external containers are responsible for including them in
            // the singular/isolated network
            Network::Isolated | Network::Singular => {
                if let Some(n) = network {
                    self.add_container_to_network(running.id(), n).await?;
                }
            }
        }

        Ok(running)
    }
    pub async fn create_dynamic_container(
        &self,
        composition: Composition,
        network: Option<&str>,
    ) -> Result<CreateDynamicContainer, DockerTestError> {
        let details = self
            .client
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
                            self.client
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
                    .running_container_from_composition(composition, d)
                    .await?;

                // Regardless of network mode the first to create a Dynamic container is
                // responsible for adding it to the network
                if let Some(n) = network {
                    self.add_container_to_network(&running.id, n).await?;
                }

                Ok(CreateDynamicContainer::RunningPrior(running))
            }
            Err(e) => match e {
                bollard::errors::Error::DockerResponseServerError {
                    message: _,
                    status_code,
                } => {
                    // The container does not exists, we have to create it.
                    if status_code == 404 {
                        let pending = self.create_container_inner(composition, network).await?;

                        if let Some(n) = network {
                            self.add_container_to_network(&pending.id, n).await?;
                        }

                        Ok(CreateDynamicContainer::Pending(pending))
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
