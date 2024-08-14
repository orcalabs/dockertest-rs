use std::{collections::HashMap, sync::Arc};

use crate::{docker::Docker, DockerTestError};
use lazy_static::lazy_static;
use tokio::sync::RwLock;

static SINGULAR_NETWORK_NAME: &str = "dockertest";

// Controls all interaction with scoped networks within a single test binary
lazy_static! {
    pub(crate) static ref SCOPED_NETWORKS: ScopedNetworks = ScopedNetworks::default();
}

#[derive(Default)]
pub struct ScopedNetworks {
    singular: Arc<RwLock<HashMap<String, SingularNetwork>>>,
}

#[derive(Debug)]
pub struct SingularNetwork {
    status: NetworkCreation,
}

// What state the singular network is currently in
#[derive(Debug)]
enum NetworkCreation {
    Failed,
    Complete(String),
}

impl ScopedNetworks {
    pub(crate) fn name(&self, namespace: &str) -> String {
        format!("{namespace}-{SINGULAR_NETWORK_NAME}")
    }
}

impl ScopedNetworks {
    pub(crate) async fn create_singular_network(
        &self,
        client: &Docker,
        self_container: Option<&str>,
        namespace: &str,
    ) -> Result<String, DockerTestError> {
        let mut networks = self.singular.write().await;

        let network_name = self.name(namespace);

        if let Some(network) = networks.get(namespace) {
            match &network.status {
                NetworkCreation::Failed => Err(DockerTestError::Startup(
                    "failed to create singular network".to_string(),
                )),
                NetworkCreation::Complete(id) => Ok(id.clone()),
            }
        } else {
            let id = if let Some(id) = client.existing_dockertest_network(&network_name).await? {
                networks.insert(
                    namespace.to_string(),
                    SingularNetwork {
                        status: NetworkCreation::Complete(id.clone()),
                    },
                );
                return Ok(id);
            } else {
                match client.create_singular_network_impl(network_name).await {
                    Ok(id) => Ok(id),
                    Err(e) => {
                        networks.insert(
                            namespace.to_string(),
                            SingularNetwork {
                                status: NetworkCreation::Failed,
                            },
                        );
                        Err(e)
                    }
                }
            }?;
            if let Some(container_id) = self_container {
                if let Err(e) = client.add_self_to_network(container_id, &id).await {
                    networks.insert(
                        namespace.to_string(),
                        SingularNetwork {
                            status: NetworkCreation::Failed,
                        },
                    );
                    return Err(e);
                }
            }
            networks.insert(
                namespace.to_string(),
                SingularNetwork {
                    status: NetworkCreation::Complete(id.to_string()),
                },
            );
            Ok(id)
        }
    }
}
