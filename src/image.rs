//! An Image persisted in Docker.

use crate::DockerTestError;
use base64::{engine::general_purpose, Engine};
use bollard::auth::DockerCredentials;
use secrecy::{ExposeSecret, Secret};
use serde::Deserialize;
use std::sync::{Arc, RwLock};
use tracing::{debug, trace};

/// Represents a docker `Image`.
///
/// This structure embeds the information related to its naming, tag and `Source` location.
#[derive(Clone, Debug)]
pub struct Image {
    pub(crate) repository: String,
    pub(crate) tag: String,
    pub(crate) source: Option<Source>,
    pull_policy: PullPolicy,
    id: Arc<RwLock<String>>,
}

/// Represents the `Source` of an `Image`.
#[derive(Clone, Debug)]
pub enum Source {
    /// Use the local docker daemon storage.
    Local,
    /// Retrieve from hub.docker.com, the official registry.
    DockerHub,
    /// Provide the domain and credentials of the Docker Registry to authenticate with.
    RegistryWithCredentials(RegistryCredentials),
    /// Use the existing credentials `docker login` credentials active for the current user,
    /// against the desired registry at the provided address.
    ///
    /// This is useful when collaborating across developers with access to the same registry,
    /// as they will be required to login with their own credentials to access the image.
    /// It may also be useful in a CI circumstance, where you only login once.
    ///
    /// Please note that the protocol portion of the address is not supplied. E.g.,
    /// * `ghcr.io`
    /// * `myregistry.azurecr.io`
    RegistryWithDockerLogin(String),
}

/// Represents credentials to a custom remote Docker Registry.
#[derive(Clone, Debug)]
pub struct RegistryCredentials {
    /// The domain (without the protocol) of the registry.
    pub address: String,
    /// Username of the credentials against the registry.
    pub username: String,
    /// Password of the credentials against the registry.
    pub password: Secret<String>,
}

/// The policy for pulling from remote locations.
#[derive(Clone, Debug)]
pub enum PullPolicy {
    /// Never pull - expect image to be present locally.
    Never,
    /// Always attempt to pull, regardless if it exists locally or not.
    Always,
    /// Check if image is present locally first, only pull if not present.
    IfNotPresent,
}

impl Image {
    /// Creates an `Image` with the given repository.
    ///
    /// The default tag is `latest` and its source is `Source::Local`.
    pub fn with_repository<T: ToString>(repository: T) -> Image {
        Image {
            repository: repository.to_string(),
            tag: "latest".to_string(),
            source: None,
            pull_policy: PullPolicy::IfNotPresent,
            id: Arc::new(RwLock::new("".to_string())),
        }
    }

    /// Set the tag for this `Image`.
    ///
    /// If left unconfigured, it will default to `latest`.
    pub fn tag<T: ToString>(self, tag: T) -> Image {
        Image {
            tag: tag.to_string(),
            ..self
        }
    }

    /// Set the [Source] for this `Image`.
    ///
    /// If left unconfigured, it will default to [Source::Local].
    pub fn source(self, source: Source) -> Image {
        Image {
            source: Some(source),
            ..self
        }
    }

    /// The the [PullPolicy] of this `Image`.
    ///
    /// If left unconfigured, it will default to [PullPolicy::IfNotPresent].
    pub fn pull_policy(self, policy: PullPolicy) -> Image {
        Image {
            pull_policy: policy,
            ..self
        }
    }

    /// Returns the repository of this `Image`.
    ///
    /// This property is often generalized as the variable `name`.
    pub(crate) fn repository(&self) -> &str {
        &self.repository
    }

    /// Returns the id of the image
    pub(crate) fn retrieved_id(&self) -> String {
        let id = self.id.read().expect("failed to get id lock");
        id.clone()
    }

    pub(crate) async fn set_id(&self, image_id: String) {
        let mut id = self.id.write().expect("failed to get id lock");
        *id = image_id;
    }

    /// Determine whether or not the `Image` should be pulled from `Source`.
    ///
    /// This function will consult the `Source`, `PullPolicy` and whether it already
    /// exists on the local docker daemon.
    pub(crate) fn should_pull(
        &self,
        exists: bool,
        source: &Source,
    ) -> Result<bool, DockerTestError> {
        match source {
            Source::RegistryWithCredentials(_) => {
                let valid = is_valid_pull_policy(exists, &self.pull_policy).map_err(|e| {
                    DockerTestError::Pull {
                        repository: self.repository.to_string(),
                        tag: self.tag.to_string(),
                        error: e,
                    }
                })?;
                Ok(valid)
            }
            Source::RegistryWithDockerLogin(_) => {
                let valid = is_valid_pull_policy(exists, &self.pull_policy).map_err(|e| {
                    DockerTestError::Pull {
                        repository: self.repository.to_string(),
                        tag: self.tag.to_string(),
                        error: e,
                    }
                })?;
                Ok(valid)
            }
            Source::DockerHub => {
                let valid = is_valid_pull_policy(exists, &self.pull_policy).map_err(|e| {
                    DockerTestError::Pull {
                        repository: self.repository.to_string(),
                        tag: self.tag.to_string(),
                        error: e,
                    }
                })?;
                Ok(valid)
            }
            Source::Local => {
                if exists {
                    Ok(false)
                } else {
                    Err(DockerTestError::Pull {
                        repository: self.repository.to_string(),
                        tag: self.tag.to_string(),
                        error: "image does not exist locally and image source is set to local"
                            .to_string(),
                    })
                }
            }
        }
    }

    /// Resolve the auth credentials based on the provided [Source].
    pub(crate) fn resolve_auth(
        &self,
        source: &Source,
    ) -> Result<Option<DockerCredentials>, DockerTestError> {
        let potential = match source {
            Source::RegistryWithDockerLogin(address) => {
                let credentials =
                    resolve_docker_login_auth(address).map_err(|e| DockerTestError::Pull {
                        repository: self.repository.to_string(),
                        tag: self.tag.to_string(),
                        error: e,
                    })?;

                Some(credentials)
            }
            Source::RegistryWithCredentials(r) => {
                let credentials = DockerCredentials {
                    username: Some(r.username.clone()),
                    password: Some(r.password.expose_secret().clone()),
                    serveraddress: Some(r.address.clone()),
                    ..Default::default()
                };

                Some(credentials)
            }
            Source::Local | Source::DockerHub => None,
        };

        Ok(potential)
    }
}

fn is_valid_pull_policy(exists: bool, pull_policy: &PullPolicy) -> Result<bool, String> {
    match pull_policy {
        PullPolicy::Never => {
            if exists {
                Ok(false)
            } else {
                Err("image does not exist locally and pull policy is set to never".to_string())
            }
        }

        PullPolicy::Always => Ok(true),

        PullPolicy::IfNotPresent => {
            if exists {
                Ok(false)
            } else {
                Ok(true)
            }
        }
    }
}

// TODO: Support existing identitytoken/registrytoken ?
#[derive(Deserialize)]
struct DockerAuthConfigEntry {
    auth: Option<String>,
}

/// Read the local cache of login access credentials
///
/// See reference:
/// https://docs.docker.com/engine/reference/commandline/login/
fn resolve_docker_login_auth(address: &str) -> Result<DockerCredentials, String> {
    // TODO: Only read this file once
    // TODO: Support windows basepath %USERPROFILE%
    let basepath = std::env::var("HOME").map_err(|e| {
        debug!("reading env `HOME` failed: {}", e);
        "unable to resolve basepath to read credentials from `docker login`: reading env `HOME`"
    })?;
    let filepath = format!("{}/.docker/config.json", basepath);
    let error = "credentials for docker registry `{}` not available";

    let file = std::fs::File::open(filepath).map_err(|e| {
        debug!(
            "resolving credentials from `docker login` failed to read file: {}",
            e
        );
        error
    })?;
    let reader = std::io::BufReader::new(file);

    let mut json: serde_json::Value = serde_json::from_reader(reader).map_err(|e| {
        debug!("parsing credentials from `docker login` failed: {}", e);
        error
    })?;

    // NOTE: There also exists a legacy auth config file format, but we don't care about this.
    let entry: DockerAuthConfigEntry = serde_json::from_value(json["auths"][address].take())
        .map_err(|e| {
            debug!(
                "no docker registry entry in credentials from `docker login` for `{}`",
                address
            );
            trace!("convertion error: {}", e);
            error
        })?;

    // The entry.auth field is base64 encoding of username:password.
    // The daemon does not support unpacking this itself, it seems.
    let auth_encoded = entry
        .auth
        .ok_or("expecting 'auth' field to be present from `docker login`")?;
    let auth_decoded = general_purpose::STANDARD
        .decode(auth_encoded)
        .map(|s| String::from_utf8_lossy(&s).to_string())
        .map_err(|e| {
            debug!(
                "decoding base64 'auth' field from `docker login` failed: {}",
                e
            );
            error
        })?;
    let (username, password) = auth_decoded.split_once(':').ok_or_else(|| {
        debug!("decoded base64 'auth' field from `docker login` does not contain expected ':' separator");
        error
    })?;

    let credentials = DockerCredentials {
        username: Some(username.to_string()),
        password: Some(password.to_string()),
        serveraddress: Some(address.to_string()),
        ..Default::default()
    };

    debug!(
        "resolved `docker login` credentials for docker registry `{}`",
        address
    );

    Ok(credentials)
}

impl RegistryCredentials {
    /// Creates a new [RegistryCredentials]
    pub fn new(address: String, username: String, password: Secret<String>) -> RegistryCredentials {
        RegistryCredentials {
            address,
            username,
            password,
        }
    }
}
