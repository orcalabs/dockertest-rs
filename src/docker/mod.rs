use crate::{utils::connect_with_local_or_tls_defaults, DockerTestError};

/// Encapsulates all docker daemon operations
#[derive(Clone, Debug)]
pub struct Docker {
    client: bollard::Docker,
}

mod composition;
mod container;
mod image;
mod network;
mod static_container;
mod volume;

pub use container::{ContainerLogSource, ContainerState, LogEntry};

impl Docker {
    pub fn new() -> Result<Self, DockerTestError> {
        let client = connect_with_local_or_tls_defaults().unwrap();

        Ok(Self { client })
    }
}
