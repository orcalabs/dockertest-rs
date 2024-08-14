use std::collections::HashMap;

use bollard::network::{CreateNetworkOptions, DisconnectNetworkOptions, ListNetworksOptions};
use tracing::{event, Level};

use crate::DockerTestError;

use super::Docker;

impl Docker {
    pub async fn create_singular_network_impl(
        &self,
        network_name: String,
    ) -> Result<String, DockerTestError> {
        let config = CreateNetworkOptions {
            name: network_name.as_str(),
            ..Default::default()
        };

        event!(Level::TRACE, "creating singular network");

        match self.client.create_network(config).await {
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
                            Ok(self
                                .existing_dockertest_network(&network_name)
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

    pub async fn existing_dockertest_network(
        &self,
        network_name: &str,
    ) -> Result<Option<String>, DockerTestError> {
        let mut filter = HashMap::with_capacity(1);
        filter.insert("name", vec![network_name]);

        let opts = ListNetworksOptions { filters: filter };
        let networks = self
            .client
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

    pub(crate) async fn create_network(
        &self,
        network_name: &str,
        self_container: Option<&str>,
    ) -> Result<(), DockerTestError> {
        let config = CreateNetworkOptions {
            name: network_name,
            ..Default::default()
        };

        event!(Level::TRACE, "creating network {}", network_name);
        let res = self
            .client
            .create_network(config)
            .await
            .map(|_| ())
            .map_err(|e| {
                DockerTestError::Startup(format!("creating docker network failed: {}", e))
            });

        event!(
            Level::TRACE,
            "finished created network with result: {}",
            res.is_ok()
        );

        if let Some(id) = self_container {
            if let Err(e) = self.add_self_to_network(id, network_name).await {
                if let Err(e) = self.client.remove_network(network_name).await {
                    event!(
                        Level::ERROR,
                        "unable to remove docker network `{}`: {}",
                        network_name,
                        e
                    );
                }
                return Err(e);
            }
        }

        res
    }

    /// Make sure we remove the network we have previously created.
    pub(crate) async fn delete_network(&self, network_name: &str, self_container: Option<&str>) {
        if let Some(id) = self_container {
            let opts = DisconnectNetworkOptions::<&str> {
                container: id,
                force: true,
            };
            if let Err(e) = self.client.disconnect_network(network_name, opts).await {
                event!(
                    Level::ERROR,
                    "unable to remove dockertest-container from network: {}",
                    e
                );
            }
        }

        if let Err(e) = self.client.remove_network(network_name).await {
            event!(
                Level::ERROR,
                "unable to remove docker network `{}`: {}",
                network_name,
                e
            );
        }
    }

    pub(crate) async fn add_self_to_network(
        &self,
        container_id: &str,
        network_name: &str,
    ) -> Result<(), DockerTestError> {
        event!(
            Level::TRACE,
            "adding dockertest container to created network, container_id: {}, network_id: {}",
            container_id,
            network_name,
        );
        let opts = bollard::network::ConnectNetworkOptions {
            container: container_id,
            endpoint_config: bollard::models::EndpointSettings::default(),
        };

        self.client
            .connect_network(network_name, opts)
            .await
            .map_err(|e| {
                DockerTestError::Startup(format!(
                    "failed to add internal container to dockertest network: {}",
                    e
                ))
            })
    }

    // TODO: isolate to static mod only
    pub async fn add_container_to_network(
        &self,
        container_id: &str,
        network: &str,
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

        match self.client.connect_network(network, opts).await {
            Ok(_) => Ok(()),
            Err(e) => match e {
                bollard::errors::Error::DockerResponseServerError {
                    status_code,
                    message: _,
                } => {
                    // The container was already connected to the network which is what we wanted
                    // anyway
                    if status_code == 403 {
                        Ok(())
                    } else {
                        Err(DockerTestError::Startup(format!(
                            "failed to add static container to dockertest network: {}",
                            e
                        )))
                    }
                }
                _ => Err(DockerTestError::Startup(format!(
                    "failed to add static container to dockertest network: {}",
                    e
                ))),
            },
        }
    }
}
