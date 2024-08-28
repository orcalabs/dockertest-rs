use super::Docker;
use crate::{
    composition::Composition, container::CreatedContainer, static_container::STATIC_CONTAINERS,
    DockerTestError, Network, PendingContainer,
};
use bollard::{
    container::{
        Config, CreateContainerOptions, InspectContainerOptions, NetworkingConfig,
        RemoveContainerOptions,
    },
    models::HostConfig,
    service::{EndpointSettings, PortBinding},
};
use std::collections::HashMap;
use tracing::{debug, event, trace, Level};

impl Docker {
    pub async fn create_container(
        &self,
        composition: Composition,
        network: Option<&str>,
        network_settings: &Network,
    ) -> Result<CreatedContainer, DockerTestError> {
        trace!("evaluating composition: {composition:#?}");
        if composition.is_static() {
            STATIC_CONTAINERS
                .create(composition, self, network, network_settings)
                .await
        } else {
            self.create_container_inner(composition, network)
                .await
                .map(CreatedContainer::Pending)
        }
    }
    // Performs container creation, should NOT be called outside of this module or the static
    // module.
    // This is only exposed such that the static module can reach it.
    // TODO: isolate to static mod only
    pub async fn create_container_inner(
        &self,
        composition: Composition,
        network: Option<&str>,
    ) -> Result<PendingContainer, DockerTestError> {
        debug!("creating container: {}", composition.container_name);

        let start_policy_clone = composition.start_policy.clone();
        let container_name_clone = composition.container_name.clone();

        if !composition.is_static() {
            // Ensure we can remove the previous container instance, if it somehow still exists.
            // Only bail on non-recoverable failure.
            match self
                .remove_container_if_exists(&composition.container_name)
                .await
            {
                Ok(_) => {}
                Err(e) => match e {
                    DockerTestError::Recoverable(_) => {}
                    _ => return Err(e),
                },
            }
        }

        let image_id = composition.image().retrieved_id();
        // Additional programming guard.
        // This Composition cannot be created without an image id, which
        // is set through `Image::pull`
        if image_id.is_empty() {
            return Err(DockerTestError::Processing("`Composition::create()` invoked without populating its image through `Image::pull()`".to_string()));
        }

        // As we can't return temporary values owned by this closure
        // we have to first convert our map into a vector of owned strings,
        // then convert it to a vector of borrowed strings (&str).
        // There is probably a better way to do this...
        let envs: Vec<String> = composition
            .env
            .iter()
            .map(|(key, value)| format!("{}={}", key, value))
            .collect();
        let envs = envs.iter().map(|s| s.as_ref()).collect();
        let cmds = composition.cmd.iter().map(|s| s.as_ref()).collect();

        let mut volumes: Vec<String> = Vec::new();
        for v in composition.bind_mounts.iter() {
            event!(
                Level::DEBUG,
                "creating host_mounted_volume: {} for container {}",
                v.as_str(),
                composition.container_name
            );
            volumes.push(v.to_string());
        }

        for v in composition.final_named_volume_names.iter() {
            event!(
                Level::DEBUG,
                "creating named_volume: {} for container {}",
                &v,
                composition.container_name
            );
            volumes.push(v.to_string());
        }

        let mut port_map: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        let mut exposed_ports: HashMap<&str, HashMap<(), ()>> = HashMap::new();

        for (exposed, host) in &composition.port {
            let dest_port: Vec<PortBinding> = vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some(host.clone()),
            }];
            port_map.insert(exposed.to_string(), Some(dest_port));
            exposed_ports.insert(exposed, HashMap::new());
        }

        let network_aliases = composition.network_aliases.as_ref();
        let mut net_config = None;

        let static_management_policy = composition.static_management_policy().clone();
        let handle = composition.handle();

        #[cfg(target_os = "linux")]
        let tmpfs: Option<HashMap<String, String>> = Some(
            composition
                .tmpfs
                .into_iter()
                .map(|v| (v, "".into()))
                .collect::<HashMap<String, String>>(),
        );

        #[cfg(not(target_os = "linux"))]
        let tmpfs = None;

        let publish_all_ports = composition.publish_all_ports;
        let privileged = composition.privileged;

        // Construct host config
        let host_config = network.map(|n| HostConfig {
            network_mode: Some(n.to_string()),
            binds: Some(volumes),
            port_bindings: Some(port_map),
            publish_all_ports: Some(publish_all_ports),
            privileged: Some(privileged),
            tmpfs,
            ..Default::default()
        });

        if let Some(n) = network {
            net_config = network_aliases.map(|a| {
                let mut endpoints = HashMap::new();
                let settings = EndpointSettings {
                    aliases: Some(a.to_vec()),
                    ..Default::default()
                };
                endpoints.insert(n, settings);
                NetworkingConfig {
                    endpoints_config: endpoints,
                }
            });
        }

        // Construct options for create container
        let options = Some(CreateContainerOptions {
            name: &composition.container_name,
            // Sets the platform of the server if its multi-platform capable, we might support user
            // provided values here at a later time.
            platform: None,
        });

        let config = Config::<&str> {
            image: Some(&image_id),
            cmd: Some(cmds),
            env: Some(envs),
            networking_config: net_config,
            host_config,
            exposed_ports: Some(exposed_ports),
            ..Default::default()
        };

        trace!("creating container from options: {options:#?}, config: {config:#?}");

        let container_info = self
            .client
            .create_container(options, config)
            .await
            .map_err(|e| DockerTestError::Daemon(format!("failed to create container: {}", e)))?;

        Ok(PendingContainer::new(
            &container_name_clone,
            container_info.id,
            handle,
            start_policy_clone,
            composition.wait,
            self.clone(),
            static_management_policy,
            composition.log_options.clone(),
        ))
    }

    // Forcefully removes the given container if it exists.
    async fn remove_container_if_exists(&self, name: &str) -> Result<(), DockerTestError> {
        self.client
            .inspect_container(name, None::<InspectContainerOptions>)
            .await
            .map_err(|e| DockerTestError::Recoverable(format!("container did not exist: {}", e)))?;

        // We were able to inspect it successfully, it exists.
        // Therefore, we can simply force remove it.
        let options = Some(RemoveContainerOptions {
            force: true,
            ..Default::default()
        });
        self.client
            .remove_container(name, options)
            .await
            .map_err(|e| {
                DockerTestError::Daemon(format!("failed to remove existing container: {}", e))
            })
    }
}
