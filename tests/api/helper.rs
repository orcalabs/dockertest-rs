use bollard::{container::InspectContainerOptions, secret::ContainerStateStatusEnum, Docker};
use dockertest::{utils::connect_with_local_or_tls_defaults, ContainerState, OperationalContainer};

pub struct TestHelper {
    client: Docker,
}

impl TestHelper {
    pub fn new() -> TestHelper {
        TestHelper {
            client: connect_with_local_or_tls_defaults().unwrap(),
        }
    }

    pub async fn container_state(&self, name: &str) -> ContainerState {
        self.client
            .inspect_container(name, None::<InspectContainerOptions>)
            .await
            .unwrap()
            .state
            .unwrap()
            .status
            .unwrap()
            .into()
    }

    pub async fn env_value(&self, handle: &OperationalContainer, env: &str) -> Option<String> {
        self.client
            .inspect_container(handle.name(), None)
            .await
            .unwrap()
            .config
            .unwrap()
            .env
            .into_iter()
            .flatten()
            .find(|v| v.split('=').collect::<Vec<_>>()[0] == env)
            .map(|v| v.split('=').collect::<Vec<_>>()[1].to_string())
    }

    pub async fn cmd(&self, handle: &OperationalContainer) -> Option<Vec<String>> {
        self.client
            .inspect_container(handle.name(), None)
            .await
            .unwrap()
            .config
            .unwrap()
            .cmd
    }
    pub async fn has_tmpfs_mount_point(
        &self,
        handle: &OperationalContainer,
        mount_point: &str,
    ) -> bool {
        self.client
            .inspect_container(handle.name(), None)
            .await
            .unwrap()
            .host_config
            .unwrap()
            .tmpfs
            .unwrap()
            .contains_key(mount_point)
    }

    pub async fn container_status(
        &self,
        handle: &OperationalContainer,
    ) -> ContainerStateStatusEnum {
        self.client
            .inspect_container(handle.name(), None)
            .await
            .unwrap()
            .state
            .unwrap()
            .status
            .unwrap()
    }
}
