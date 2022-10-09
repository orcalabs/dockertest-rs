use std::{collections::HashMap, sync::Arc};

use crate::{runner::add_self_to_network, DockerTestError};
use bollard::{
    network::{CreateNetworkOptions, ListNetworksOptions},
    Docker,
};
use lazy_static::lazy_static;
use tokio::sync::RwLock;
use tracing::{event, Level};

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
            let id = if let Some(id) = existing_dockertest_network(client, &network_name).await? {
                networks.insert(
                    namespace.to_string(),
                    SingularNetwork {
                        status: NetworkCreation::Complete(id.clone()),
                    },
                );
                return Ok(id);
            } else {
                match create_singular_network_impl(client, network_name).await {
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
                if let Err(e) = add_self_to_network(client, container_id, &id).await {
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

async fn create_singular_network_impl(
    client: &Docker,
    network_name: String,
) -> Result<String, DockerTestError> {
    let config = CreateNetworkOptions {
        name: network_name.as_str(),
        ..Default::default()
    };

    event!(Level::TRACE, "creating singular network");

    match client.create_network(config).await {
        Ok(resp) => match resp.id {
            Some(id) => Ok(id),
            None => Err(DockerTestError::Startup(
                "failed to get id of singular network".to_string(),
            )),
        },
        Err(e) => {
            match e {
                bollard::errors::Error::DockerResponseServerError {
                    status_code,
                    message,
                } => {
                    if status_code == 409 {
                        // We assume that we got a conflict due to multiple networks with the name
                        // `dockertest`, and therefore assume that 'existing_dockertest_network' will
                        // return the conflicting network.
                        Ok(existing_dockertest_network(client, &network_name)
                            .await?
                            .unwrap())
                    } else {
                        Err(DockerTestError::Startup(format!(
                            "failed to create singular network: {message}",
                        )))
                    }
                }
                _ => Err(DockerTestError::Startup(format!(
                    "failed to create singular network: {e}"
                ))),
            }
        }
    }
}

async fn existing_dockertest_network(
    client: &Docker,
    network_name: &str,
) -> Result<Option<String>, DockerTestError> {
    let mut filter = HashMap::with_capacity(1);
    filter.insert("name", vec![network_name]);

    let opts = ListNetworksOptions { filters: filter };
    let networks = client
        .list_networks(Some(opts))
        .await
        .map_err(|e| DockerTestError::Startup(format!("failed to list networks: {e}")))?;

    let mut highest_timestamp: Option<String> = None;
    let mut highest_timestamp_id: Option<String> = None;

    // We assume id is present on the returned networks
    for n in networks {
        if let Some(name) = n.name {
            if name == network_name {
                if let Some(timestamp) = n.created {
                    if let Some(compare_timestamp) = &highest_timestamp {
                        if timestamp.as_str() > compare_timestamp.as_str() {
                            highest_timestamp = Some(timestamp);
                            highest_timestamp_id = Some(n.id.unwrap());
                        }
                    } else {
                        highest_timestamp = Some(timestamp);
                        highest_timestamp_id = Some(n.id.unwrap());
                    }
                }
            }
        }
    }

    Ok(highest_timestamp_id)
}
