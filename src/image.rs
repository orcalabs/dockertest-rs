//! An Image persisted in Docker.

use crate::DockerTestError;

use bollard::{
    auth::DockerCredentials, errors::Error, image::CreateImageOptions, models::CreateImageInfo,
    Docker,
};

use base64::{engine::general_purpose, Engine};
use futures::stream::StreamExt;
use secrecy::{ExposeSecret, Secret};
use serde::Deserialize;
use tracing::{debug, event, trace, Level};

use std::rc::Rc;
use std::sync::RwLock;

/// Represents a docker `Image`.
///
/// This structure embeds the information related to its naming, tag and `Source` location.
#[derive(Clone, Debug)]
pub struct Image {
    repository: String,
    tag: String,
    source: Option<Source>,
    pull_policy: PullPolicy,
    id: Rc<RwLock<String>>,
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
            id: Rc::new(RwLock::new("".to_string())),
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

    // Pulls the image from its source with the given docker client.
    // NOTE(lint): uncertain how to structure this otherwise
    #[allow(clippy::match_single_binding)]
    async fn do_pull(
        &self,
        client: &Docker,
        auth: Option<DockerCredentials>,
    ) -> Result<(), DockerTestError> {
        debug!("pulling image: {}:{}", self.repository, self.tag);
        let options = Some(CreateImageOptions::<&str> {
            from_image: &self.repository,
            tag: &self.tag,
            ..Default::default()
        });

        let mut stream = client.create_image(options, None, auth);
        // This stream will intermittently yield a progress update.
        while let Some(result) = stream.next().await {
            match result {
                Ok(intermitten_result) => match intermitten_result {
                    CreateImageInfo {
                        id,
                        error,
                        error_detail,
                        status,
                        progress,
                        progress_detail,
                    } => {
                        if error.is_some() {
                            event!(
                                Level::ERROR,
                                "pull error {} {:?}",
                                error.clone().unwrap(),
                                error_detail.unwrap_or_default()
                            );
                        } else {
                            event!(
                                Level::TRACE,
                                "pull progress {} {:?} {:?} {:?}",
                                status.clone().unwrap_or_default(),
                                id.clone().unwrap_or_default(),
                                progress.clone().unwrap_or_default(),
                                progress_detail.clone().unwrap_or_default()
                            );
                        }
                    }
                },
                Err(e) => {
                    let msg = match e {
                        Error::DockerResponseServerError {
                            message: _,
                            status_code,
                        } => {
                            if status_code == 404 {
                                "unknown registry or image".to_string()
                            } else {
                                e.to_string()
                            }
                        }
                        _ => e.to_string(),
                    };
                    return Err(DockerTestError::Pull {
                        repository: self.repository.to_string(),
                        tag: self.tag.to_string(),
                        error: msg,
                    });
                }
            }
        }

        // TODO: Verify that we have actually pulled the image.
        // NOTE: The engine may return a 500 when we unauthorized, but bollard does not give us
        // this failure. Rather, it just does not provide anthing in the stream.
        // If a repo is submitted that we do not have access to, and no auth is supplied,
        // we will no error.

        event!(Level::DEBUG, "successfully pulled image");
        Ok(())
    }

    // Retrieves the id of the image from the local docker daemon and
    // sets that id field in image to that value.
    // If this method is invoked and the image does not exist locally,
    // it will return an error.
    async fn retrieve_and_set_id(&self, client: &Docker) -> Result<(), DockerTestError> {
        match client
            .inspect_image(&format!("{}:{}", self.repository, self.tag))
            .await
        {
            Ok(details) => {
                let mut id = self.id.write().expect("failed to get id lock");
                *id = details.id.expect("image did not have an id");
                Ok(())
            }
            Err(e) => {
                event!(
                    Level::TRACE,
                    "failed to retrieve ID of image: {}, tag: {}, source: {:?} ",
                    self.repository,
                    self.tag,
                    self.source
                );
                Err(DockerTestError::Pull {
                    repository: self.repository.to_string(),
                    tag: self.tag.to_string(),
                    error: e.to_string(),
                })
            }
        }
    }

    /// Checks whether the image exists locally through attempting to inspect it.
    ///
    /// If docker daemon communication failed, we will also implicitly return false.
    async fn does_image_exist(&self, client: &Docker) -> Result<bool, DockerTestError> {
        match client
            .inspect_image(&format!("{}:{}", self.repository, self.tag))
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => match e {
                Error::DockerResponseServerError {
                    message: _,
                    status_code,
                } => {
                    if status_code == 404 {
                        Ok(false)
                    } else {
                        Err(DockerTestError::Daemon(e.to_string()))
                    }
                }
                _ => Err(DockerTestError::Daemon(e.to_string())),
            },
        }
    }

    /// Pulls the `Image` if neccessary.
    ///
    /// This function respects the `Image` Source and PullPolicy settings.
    pub(crate) async fn pull(
        &self,
        client: &Docker,
        default_source: &Source,
    ) -> Result<(), DockerTestError> {
        let pull_source = match &self.source {
            None => default_source,
            Some(r) => r,
        };

        let exists = self.does_image_exist(client).await?;

        if self.should_pull(exists, pull_source)? {
            let auth = self.resolve_auth(pull_source)?;
            self.do_pull(client, auth).await?;
        }

        // FIXME: If we encounter a scenario where the image should not be pulled, we need to err
        // with appropriate information. Currently, it fails with the same error message as
        // other scenarios.
        self.retrieve_and_set_id(client).await
    }

    /// Determine whether or not the `Image` should be pulled from `Source`.
    ///
    /// This function will consult the `Source`, `PullPolicy` and whether it already
    /// exists on the local docker daemon.
    fn should_pull(&self, exists: bool, source: &Source) -> Result<bool, DockerTestError> {
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
    fn resolve_auth(&self, source: &Source) -> Result<Option<DockerCredentials>, DockerTestError> {
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

#[cfg(test)]
mod tests {
    use crate::image::{Image, PullPolicy, RegistryCredentials, Source};
    use crate::utils::connect_with_local_or_tls_defaults;
    use crate::{test_utils, test_utils::CONCURRENT_IMAGE_ACCESS_QUEUE};

    use secrecy::Secret;

    /// Tests that our `Image::pull` method succeeds with a valid `Source::Local` repository image.
    /// After the pull operation has been performed the image id shall be sat on the object.
    #[tokio::test]
    async fn test_pull_succeeds_with_valid_local_source() {
        let client = connect_with_local_or_tls_defaults().unwrap();

        let repository = "dockertest-rs/hello";
        let tag = "latest";
        let image = Image::with_repository(repository);

        // This repository image exists locally (pre-requisuite build.rs)
        assert!(test_utils::image_exists_locally(&client, repository, tag)
            .await
            .unwrap());

        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should not be set before pulling"
        );

        // Attempting to pull the image from local source should success, with
        // its id populated.
        let result = image.pull(&client, &Source::Local).await;
        assert!(
            result.is_ok(),
            "should not fail pull process with local source and existing image: {}",
            result.unwrap_err()
        );

        let expected_id = test_utils::image_id(&client, repository, tag)
            .await
            .expect("failed to get image_id");
        assert_eq!(
            image.retrieved_id(),
            expected_id,
            "the id set for image does not equal the expected value"
        );
    }

    /// Tests that when attempting to `Image::pull` on an non-existing image with
    /// `PullPolicy::Local`, the operation fails with a descriptive error message.
    #[tokio::test]
    async fn test_pull_fails_with_invalid_local_source() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = CONCURRENT_IMAGE_ACCESS_QUEUE.access().await;
        let tag = "latest";
        let image = Image::with_repository(*repository).tag(tag);
        let result = test_utils::delete_image_if_present(&client, *repository, tag).await;
        assert!(
            result.is_ok(),
            "failed to delete image: {}",
            result.unwrap_err()
        );

        // Pull operation should fail since local pull on non-existent image
        let result = image.pull(&client, &Source::Local).await;
        assert!(result.is_err());
    }

    /// Tests that our exposed pull method succeeds with a valid remote source.
    #[tokio::test]
    async fn test_pull_succeeds_with_valid_remote_source() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = CONCURRENT_IMAGE_ACCESS_QUEUE.access().await;
        let tag = "latest";
        let source = Source::DockerHub;
        let image = Image::with_repository(*repository).tag(tag);

        let result = test_utils::delete_image_if_present(&client, *repository, tag).await;
        assert!(
            result.is_ok(),
            "failed to delete image: {}",
            result.unwrap_err()
        );
        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should not be set before pulling"
        );

        let result = image.pull(&client, &source).await;
        assert!(
            result.is_ok(),
            "should not fail pull process with valid remote source: {}",
            result.unwrap_err()
        );

        let expected_id = test_utils::image_id(&client, *repository, tag)
            .await
            .expect("failed to get image_id");

        assert_eq!(
            image.retrieved_id(),
            expected_id,
            "the id set for image does not equal the expected value"
        );
    }

    /// Tests that `Image::pull` fails with an invalid remote source.
    #[tokio::test]
    async fn test_pull_fails_with_invalid_remote_source() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let remote = RegistryCredentials::new(
            "X".to_string(),
            "Y".to_string(),
            Secret::new("Z".to_string()),
        );
        let source = Source::RegistryWithCredentials(remote);

        let tag = "this_is_not_a_tag";
        let repository = "dockertest_pull_fails_with_invalid_remote_source";
        let image = Image::with_repository(repository).tag(tag);

        let result = image.pull(&client, &source).await;
        assert!(
            result.is_err(),
            "should fail pull process with an invalid remote source",
        );
    }

    // Tests that the retrieve_and_set_id method sets the image id
    #[tokio::test]
    async fn test_set_image_id() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = "dockertest-rs/hello";
        let tag = "latest";
        let image = Image::with_repository(repository).tag(tag);

        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should be empty before pulling"
        );
        let image_id = test_utils::image_id(&client, repository, tag)
            .await
            .expect("failed to get image id");

        let res = image.retrieve_and_set_id(&client).await;
        assert!(res.is_ok(), "failed to set image id: {}", res.unwrap_err());

        assert_eq!(
            image.retrieved_id(),
            image_id,
            "the id set for image does not equal the expected value"
        );
    }

    // Tests that we can check if an image exists locally with the does_image_exist method.
    #[tokio::test]
    async fn test_image_existence() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = CONCURRENT_IMAGE_ACCESS_QUEUE.access().await;
        let tag = "latest";
        let image = Image::with_repository(*repository).tag(tag);

        // Ensure image is present first
        // If it is already present, we skip the download time.
        image
            .pull(&client, &Source::DockerHub)
            .await
            .expect("failed to pull image");

        // Test image is present
        let result = image.does_image_exist(&client).await;
        assert!(
            result.is_ok(),
            "failure checking image exists {}",
            result.unwrap_err()
        );
        assert!(
            result.unwrap(),
            "should return false when image does not exist locally"
        );

        // Remove the image for the next existence test
        test_utils::delete_image_if_present(&client, *repository, tag)
            .await
            .expect("failed to remove image");

        // Test image is not present
        let result = image.does_image_exist(&client).await;
        assert!(
            result.is_ok(),
            "failure checking image exists {}",
            result.unwrap_err()
        );
        assert!(
            !result.unwrap(),
            "should return false when image does not exist locally"
        );
    }

    /// Tests that the `Image::pull` method succesfully pulls a image from DockerHub.
    #[tokio::test]
    async fn test_pull_remote_image_from_dockerhub() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = CONCURRENT_IMAGE_ACCESS_QUEUE.access().await;
        let tag = "latest";
        let image = Image::with_repository(*repository).tag(tag);

        test_utils::delete_image_if_present(&client, *repository, tag)
            .await
            .expect("failed to delete image");

        let result = image.pull(&client, &Source::DockerHub).await;
        assert!(
            result.is_ok(),
            "failed to pull image: {}",
            result.unwrap_err()
        );

        match test_utils::image_exists_locally(&client, *repository, tag).await {
            Ok(exists) => {
                assert!(
                    exists,
                    "image should exist on local daemon after pull operation"
                );
            }
            Err(e) => panic!("failed to check image existence: {}", e),
        }
    }

    // Tests the behaviour of the should_pull method with the
    // Source set to local.
    #[test]
    fn test_should_pull_local_source() {
        let source = Source::Local;
        let repository = "hello-world".to_string();
        let image = Image::with_repository(repository);

        let exists_locally = true;
        let res = image
            .should_pull(exists_locally, &source)
            .expect("returned error with image locally and source set to local");

        assert!(
            !res,
            "should not pull with image already on the local host and source set to local"
        );

        let exists_locally = false;
        let res = image.should_pull(exists_locally, &source);
        assert!(
            res.is_err(),
            "should return an error without image on local host and source set to local"
        );
    }

    // Tests the behaviour of the should_pull method with the
    // source set to remote and pull_policy set to always
    #[test]
    fn test_should_pull_remote_source_always() {
        let source = Source::DockerHub;
        let repository = "hello-world".to_string();
        let image = Image::with_repository(repository).pull_policy(PullPolicy::Always);

        let exists_locally = false;
        let res = image.should_pull(exists_locally, &source).expect("should not return an error when source is remote, pull_policy is always, and image does not exist locally");
        assert!(res, "should pull when pull policy is set to always");

        let exists_locally = true;
        let res = image.should_pull(exists_locally, &source).expect("should not return an error when source is remote, pull_policy is always, and image exists locally");
        assert!(res, "should pull when pull policy is set to always");
    }

    // Tests the behaviour of the should_pull method with the
    // source set to remote and pull_policy set to never
    #[test]
    fn test_should_pull_remote_source_never() {
        let source = Source::DockerHub;
        let repository = "hello-world".to_string();
        let image = Image::with_repository(repository).pull_policy(PullPolicy::Never);

        let exists_locally = false;
        let res = image.should_pull(exists_locally, &source);
        assert!(res.is_err(), "should return an error when pull policy is set to never and image does not exist locally");

        let exists_locally = true;
        let res = image
            .should_pull(exists_locally, &source)
            .expect("should not return an error with existing image and pull policy set to never");
        assert!(!res, "should not pull with pull_policy set to never");
    }

    // Tests the behaviour of the should_pull method with the
    // source set to remote and pull_policy set to if_not_present
    #[test]
    fn test_should_pull_remote_source_if_not_present() {
        let source = Source::DockerHub;
        let repository = "hello-world".to_string();
        let image = Image::with_repository(repository);

        let exists_locally = true;
        let res = image.should_pull(exists_locally, &source).expect(
            "should not return an error with IfNotPresent pull policy and image existing locally",
        );
        assert!(!res, "should not pull when the image already exists");

        let exists_locally = false;
        let res = image.should_pull(exists_locally, &source).expect(
            "should not return an error with IfNotPresent pull policy and image not existing locally",
        );
        assert!(res, "should pull when the image does not exist locally");
    }

    // Tests all methods for creating and manipulating an image
    #[test]
    fn test_creating_image() {
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

        let equal = image.source.is_none();
        assert!(equal, "should not have source set as default");
        assert_eq!(image.tag, "latest", "should have latest as default tag");
        assert_eq!(
            image.repository, repository,
            "wrong repository set in image creation"
        );
        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should not be set before we pull the image"
        );

        let remote = RegistryCredentials::new(
            "X".to_string(),
            "Y".to_string(),
            Secret::new("Z".to_string()),
        );
        let image = image.source(Source::RegistryWithCredentials(remote));

        let new_tag = "this_is_a_test_tag";
        let image = image.tag(new_tag);
        assert_eq!(image.tag, new_tag, "changing tag does not change image tag");
    }

    // Tests that image with a provided source overrides the default_source that is set.
    // This is tested by providing an unvalid default_source and trying to override it with a valid source by
    // providing a source to the image builder.
    // Then if the pull succeeds the default_source has been overriden by the source provided in
    // the image builder.
    // If the pull fails the, the default_source was not overriden and was used in the pull.
    // A slight downside by this setup is that the test will fail if pulling from the valid source
    // fails.
    // Could not come up with a better way to test this scenario.
    #[tokio::test]
    async fn test_image_source_overrides_default_source_in_pull() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = CONCURRENT_IMAGE_ACCESS_QUEUE.access().await;
        let tag = "latest";
        let image = Image::with_repository(*repository)
            .tag(tag)
            // Source is overridden to be DockerHub, a valid source.
            .source(Source::DockerHub);

        // Deinve a custom Remove registry that is invalid.
        let invalid_source = Source::RegistryWithCredentials(RegistryCredentials::new(
            "X".to_string(),
            "Y".to_string(),
            Secret::new("Z".to_string()),
        ));

        // Cleanup to force pull operation
        test_utils::delete_image_if_present(&client, *repository, tag)
            .await
            .expect("failed to delete image");

        // Perform operation with a invalid default source
        let result = image.pull(&client, &invalid_source).await;
        assert!(
            result.is_ok(),
            "the invalid source should be overriden by the provided valid source,
            which should result in a successful pull"
        );
    }
}
