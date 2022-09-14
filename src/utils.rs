//! Provides helper utilities used by both our crate and our integration tests.
//! Functions publically exposed here are part of a doc(hidden) module.

use crate::error::DockerTestError;

use bollard::Docker;
use rand::{self, Rng};

#[cfg(feature = "tls")]
use std::env;

#[doc(hidden)]
/// Connect to a Docker daemon with defaults
///
/// if `tls` feature is enabled and DOCKER_TLS_VERIFY env variable is set then connection is done via TLS over tcp
/// Otherwise connection is done through local unix socket or named pipe (on Windows)
pub fn connect_with_local_or_tls_defaults() -> Result<Docker, DockerTestError> {
    #[cfg(feature = "tls")]
    if let Ok(ref verify) = env::var("DOCKER_TLS_VERIFY") {
        if !verify.is_empty() {
            Docker::connect_with_ssl_defaults().map_err(|e| {
                DockerTestError::Daemon(format!("connection with TLS defaults: {:?}", e))
            })
        } else {
            Docker::connect_with_local_defaults().map_err(|e| {
                DockerTestError::Daemon(format!("connection with local defaults: {:?}", e))
            })
        }
    } else {
        Docker::connect_with_local_defaults().map_err(|e| {
            DockerTestError::Daemon(format!("connection with local defaults: {:?}", e))
        })
    }

    #[cfg(not(feature = "tls"))]
    Docker::connect_with_local_defaults()
        .map_err(|e| DockerTestError::Daemon(format!("connection with locals defaults: {:?}", e)))
}

#[doc(hidden)]
pub fn generate_random_string(len: i32) -> String {
    let mut random_string = String::new();
    let mut rng = rand::thread_rng();
    for _i in 0..len {
        let letter: char = rng.gen_range(b'a'..=b'z') as char;
        random_string.push(letter);
    }

    random_string
}
