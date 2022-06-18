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
    singular: Arc<RwLock<SingularNetwork>>,
}

#[derive(Default)]
pub struct SingularNetwork {
    status: NetworkCreation,
}

// What state the singular network is currently in
#[derive(Default)]
enum NetworkCreation {
    #[default]
    NotPerformed,
    Failed,
    Complete(String),
}

impl ScopedNetworks {
    pub(crate) fn name(&self) -> &'static str {
        SINGULAR_NETWORK_NAME
    }
}

impl ScopedNetworks {
    pub(crate) async fn create_singular_network(
        &self,
        client: &Docker,
        self_container: Option<&str>,
    ) -> Result<String, DockerTestError> {
        let mut network = self.singular.write().await;

        match &network.status {
            NetworkCreation::NotPerformed => {
                let id = if let Some(id) = existing_dockertest_network(client).await? {
                    return Ok(id);
                } else {
                    match create_singular_network_impl(client).await {
                        Ok(id) => Ok(id),
                        Err(e) => {
                            network.status = NetworkCreation::Failed;
                            Err(e)
                        }
                    }
                }?;
                if let Some(container_id) = self_container {
                    if let Err(e) = add_self_to_network(client, container_id, &id).await {
                        network.status = NetworkCreation::Failed;
                        return Err(e);
                    }
                }
                network.status = NetworkCreation::Complete(id.clone());
                Ok(id)
            }
            NetworkCreation::Failed => Err(DockerTestError::Startup(
                "failed to create singular network".to_string(),
            )),
            NetworkCreation::Complete(id) => Ok(id.clone()),
        }
    }
}

async fn create_singular_network_impl(client: &Docker) -> Result<String, DockerTestError> {
    let config = CreateNetworkOptions {
        name: SINGULAR_NETWORK_NAME,
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
                        Ok(existing_dockertest_network(client).await?.unwrap())
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

async fn existing_dockertest_network(client: &Docker) -> Result<Option<String>, DockerTestError> {
    let mut filter = HashMap::with_capacity(1);
    filter.insert("name", vec![SINGULAR_NETWORK_NAME]);

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
            if name == SINGULAR_NETWORK_NAME {
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
