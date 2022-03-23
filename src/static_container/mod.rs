use crate::{
    container::{CreatedContainer, HostPortMappings},
    Composition, DockerTestError, PendingContainer, RunningContainer, StaticManagementPolicy,
};
use dynamic::DynamicContainers;
use external::ExternalContainers;
use internal::InternalContainers;

use bollard::{
    container::RemoveContainerOptions, models::ContainerInspectResponse,
    network::DisconnectNetworkOptions, Docker,
};
use lazy_static::lazy_static;
use std::collections::HashSet;
use tracing::{event, Level};

mod dynamic;
mod external;
mod internal;

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
#[derive(Default)]
pub struct StaticContainers {
    internal: InternalContainers,
    external: ExternalContainers,
    dynamic: DynamicContainers,
}

impl StaticContainers {
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
                    .internal
                    .create(composition, client, network)
                    .await
                    .map(CreatedContainer::Pending),
                StaticManagementPolicy::External => {
                    let external = self
                        .external
                        .include(
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
                    self.dynamic.create(composition, client, network).await
                }
            }
        } else {
            Err(DockerTestError::Startup(
                "tried to create static container without a management policy".to_string(),
            ))
        }
    }

    pub async fn external_containers(&self) -> Vec<RunningContainer> {
        let mut external = self.external.containers().await;
        // Dynamic containers that were running prior to test invocation are managed the same way
        // as external containers
        let mut dynamic_running_prior = self.dynamic.prior_running_containers().await;

        external.append(&mut dynamic_running_prior);

        external
    }

    pub async fn start(
        &self,
        container: &PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        if let Some(policy) = &container.static_management_policy {
            match policy {
                StaticManagementPolicy::External => Err(DockerTestError::Startup(
                    "tried to start an external container".to_string(),
                )),
                StaticManagementPolicy::Internal => self.internal.start(container).await,
                StaticManagementPolicy::Dynamic => self.dynamic.start(container).await,
            }
        } else {
            Err(DockerTestError::Startup(
                "tried to start a non-exiting static container".to_string(),
            ))
        }
    }

    pub async fn cleanup(
        &self,
        client: &Docker,
        network: &str,
        is_external_network: bool,
        to_cleanup: Vec<&str>,
    ) {
        let cleanup_map: HashSet<&str> = to_cleanup.into_iter().collect();
        self.internal.cleanup(client, network, &cleanup_map).await;
        self.dynamic.disconnect(client, network).await;
        self.external
            .disconnect(client, network, is_external_network, &cleanup_map)
            .await;
    }
}

async fn add_to_network(
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

async fn remove_container(id: &str, client: &Docker) {
    let remove_opts = Some(RemoveContainerOptions {
        force: true,
        ..Default::default()
    });

    if let Err(e) = client.remove_container(id, remove_opts).await {
        event!(Level::ERROR, "failed to remove static container: {}", e);
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

async fn running_container_from_composition(
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
