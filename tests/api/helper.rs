use bollard::Docker;
use dockertest::{utils::connect_with_local_or_tls_defaults, OperationalContainer};

pub struct TestHelper {
    client: Docker,
}

impl TestHelper {
    pub fn new() -> TestHelper {
        TestHelper {
            client: connect_with_local_or_tls_defaults().unwrap(),
        }
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
}
