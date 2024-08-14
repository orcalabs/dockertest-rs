use crate::{
    composition::{Composition, StaticManagementPolicy},
    container::CreatedContainer,
    docker::Docker,
    DockerTestError, Network, PendingContainer, RunningContainer,
};
use dynamic::DynamicContainers;
use external::ExternalContainers;
use internal::InternalContainers;

use lazy_static::lazy_static;
use std::collections::HashSet;

mod dynamic;
mod external;
mod internal;
mod network;

pub use dynamic::CreateDynamicContainer;
pub(crate) use network::SCOPED_NETWORKS;

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
        network_mode: &Network,
    ) -> Result<CreatedContainer, DockerTestError> {
        if let Some(policy) = composition.static_management_policy() {
            match policy {
                StaticManagementPolicy::Internal => self
                    .internal
                    .create(composition, client, network, network_mode)
                    .await
                    .map(CreatedContainer::Pending),
                StaticManagementPolicy::External => {
                    let external = self
                        .external
                        .include(composition, client, network, network_mode)
                        .await?;
                    Ok(CreatedContainer::StaticExternal(external))
                }
                StaticManagementPolicy::Dynamic => {
                    self.dynamic
                        .create(composition, client, network, network_mode)
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
        network_mode: &Network,
        to_cleanup: Vec<&str>,
    ) {
        let cleanup: HashSet<&str> = to_cleanup.into_iter().collect();
        self.internal.cleanup(client, network, &cleanup).await;
        self.dynamic
            .disconnect(client, network, network_mode, &cleanup)
            .await;
        self.external
            .disconnect(client, network, network_mode, &cleanup)
            .await;
    }
}
