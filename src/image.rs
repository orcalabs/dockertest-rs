//! An Image persisted in Docker.

use crate::DockerTestError;

use bollard::{errors::Error, image::CreateImageOptions, models::CreateImageInfo, Docker};

use futures::stream::StreamExt;
use std::rc::Rc;
use std::sync::RwLock;
use tracing::{event, Level};

/// Represents a docker `Image`.
///
/// This structure embeds the information related to its naming, tag and `Source` location.
#[derive(Clone)]
pub struct Image {
    repository: String,
    tag: String,
    source: Option<Source>,
    id: Rc<RwLock<String>>,
}

/// Represents the `Source` of an `Image`.
#[derive(Clone, Debug)]
pub enum Source {
    /// Use the local docker daemon storage.
    Local,
    /// Retrieve from hub.docker.com, the official registry.
    DockerHub(PullPolicy),
    /// Custom registry address, described by the Remote structure.
    Remote(Remote),
}

/// Represents a custom remote docker registry.
///
/// NOTE: This is currently not properly supported.
#[derive(Clone, Debug)]
pub struct Remote {
    address: String,
    pull_policy: PullPolicy,
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
    /// Creates an `Image` with the given `repository`.
    ///
    /// The default tag is `latest` and its source is `Source::Local`.
    pub fn with_repository<T: ToString>(repository: T) -> Image {
        Image {
            repository: repository.to_string(),
            tag: "latest".to_string(),
            source: None,
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

    /// Set the `Source` for this `Image`.
    ///
    /// If left unconfigured, it will default to `Source::Local`.
    pub fn source(self, source: Source) -> Image {
        Image {
            source: Some(source),
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
    async fn do_pull(&self, client: &Docker) -> Result<(), DockerTestError> {
        let options = Some(CreateImageOptions::<&str> {
            from_image: &self.repository,
            repo: &self.repository,
            tag: &self.tag,
            ..Default::default()
        });

        let mut stream = client.create_image(options, None, None);
        // This stream will intermittently yield a progress update.
        while let Some(result) = stream.next().await {
            match result {
                Ok(intermitten_result) => {
                    match intermitten_result {
                        CreateImageInfo {
                            id,
                            error,
                            status,
                            progress,
                            progress_detail,
                        } => {
                            if error.is_some() {
                                event!(Level::ERROR, "pull error {}", error.clone().unwrap(),);
                            } else {
                                event!(
                                    Level::TRACE,
                                    "pull progress {} {:?} {:?} {:?}",
                                    status.clone().unwrap(),
                                    id.clone().unwrap(),
                                    progress.clone().unwrap(),
                                    progress_detail.clone().unwrap()
                                );
                            }
                        }
                    }
                    event!(Level::DEBUG, "successfully pulled image");
                }
                Err(e) => {
                    let msg = match e {
                        Error::DockerResponseNotFoundError { .. } => {
                            "unknown registry or image".to_string()
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
                *id = details.id;
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
                return Err(DockerTestError::Pull {
                    repository: self.repository.to_string(),
                    tag: self.tag.to_string(),
                    error: e.to_string(),
                });
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
                Error::DockerResponseNotFoundError { .. } => Ok(false),
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

        let pull = self.should_pull(exists, &pull_source)?;
        if pull {
            self.do_pull(client).await?;
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
            Source::Remote(r) => {
                is_valid_pull_policy(exists, r.pull_policy()).map_err(|e| DockerTestError::Pull {
                    repository: self.repository.to_string(),
                    tag: self.tag.to_string(),
                    error: e,
                })
            }
            Source::DockerHub(p) => {
                is_valid_pull_policy(exists, p).map_err(|e| DockerTestError::Pull {
                    repository: self.repository.to_string(),
                    tag: self.tag.to_string(),
                    error: e,
                })
            }
            Source::Local => {
                if exists {
                    Ok(false)
                } else {
                    Err(DockerTestError::Pull {
                        repository: self.repository.to_string(),
                        tag: self.tag.to_string(),
                        error: "image source was set to local, but the provided image does not exists on the local host".to_string(),
                    })
                }
            }
        }
    }
}

fn is_valid_pull_policy(exists: bool, pull_policy: &PullPolicy) -> Result<bool, String> {
    match pull_policy {
        PullPolicy::Never => {
            if exists {
                Ok(false)
            } else {
                Err("image source was set to remote and pull_policy to never, but the provided image does not exists on the local host".to_string())
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

impl Remote {
    /// Creates a new remote with the given address and `PullPolicy`.
    pub fn new<T: ToString>(address: &T, pull_policy: PullPolicy) -> Remote {
        Remote {
            address: address.to_string(),
            pull_policy,
        }
    }

    /// Returns the `PullPolicy associated with  this `Remote`.
    pub(crate) fn pull_policy(&self) -> &PullPolicy {
        &self.pull_policy
    }
}

#[cfg(test)]
mod tests {
    use crate::image::{Image, PullPolicy, Remote, Source};
    use crate::{test_utils, test_utils::CONCURRENT_IMAGE_ACCESS_QUEUE};

    use bollard::Docker;

    /// Tests that our `Image::pull` method succeeds with a valid `Source::Local` repository image.
    /// After the pull operation has been performed the image id shall be sat on the object.
    #[tokio::test]
    async fn test_pull_succeeds_with_valid_local_source() {
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");

        let repository = "dockertest-rs/hello";
        let tag = "latest";
        let image = Image::with_repository(&repository);

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
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");
        let repository = CONCURRENT_IMAGE_ACCESS_QUEUE.access().await;
        let tag = "latest";
        let image = Image::with_repository(*repository).tag(&tag);
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
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");
        let repository = CONCURRENT_IMAGE_ACCESS_QUEUE.access().await;
        let tag = "latest";
        let source = Source::DockerHub(PullPolicy::Always);
        let image = Image::with_repository(*repository).tag(&tag);

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

        let expected_id = test_utils::image_id(&client, *repository, &tag)
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
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");
        let remote = Remote::new(&"example.com".to_string(), PullPolicy::Always);
        let source = Source::Remote(remote);

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
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");
        let repository = "dockertest-rs/hello";
        let tag = "latest";
        let image = Image::with_repository(&repository).tag(&tag);

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
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");
        let repository = CONCURRENT_IMAGE_ACCESS_QUEUE.access().await;
        let tag = "latest";
        let image = Image::with_repository(*repository).tag(&tag);

        // Ensure image is present first
        // If it is already present, we skip the download time.
        image
            .pull(&client, &Source::DockerHub(PullPolicy::Always))
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
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");
        let repository = CONCURRENT_IMAGE_ACCESS_QUEUE.access().await;
        let tag = "latest";
        let image = Image::with_repository(*repository).tag(&tag);

        test_utils::delete_image_if_present(&client, *repository, &tag)
            .await
            .expect("failed to delete image");

        let result = image
            .pull(&client, &Source::DockerHub(PullPolicy::Always))
            .await;
        assert!(
            result.is_ok(),
            "failed to pull image: {}",
            result.unwrap_err()
        );

        match test_utils::image_exists_locally(&client, *repository, &tag).await {
            Ok(exists) => {
                assert!(
                    exists,
                    "image should exist on local daemon after pull operation"
                );
            }
            Err(e) => panic!("failed to check image existence: {}", e),
        }
    }

    // Tests that we can new up a remote with correct variables
    #[test]
    fn test_new_remote() {
        let address = "this_is_a_remote_registry".to_string();
        let pull_policy = PullPolicy::Always;
        let remote = Remote::new(&address, pull_policy);

        let equal = match remote.pull_policy {
            PullPolicy::Always => true,
            _ => false,
        };

        assert_eq!(
            address, remote.address,
            "remote was created with incorrect address"
        );
        assert!(equal, "remote was created with incorrect pull policy");

        // Assert that the getter returns the correct value
        let equal = match remote.pull_policy() {
            PullPolicy::Always => true,
            _ => false,
        };

        assert!(equal, "remote get method returned incorrect value");
    }

    // Tests the behaviour of the should_pull method with the
    // Source set to local.
    #[test]
    fn test_should_pull_local_source() {
        let source = Source::Local;
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

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
        let source = Source::DockerHub(PullPolicy::Always);
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

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
        let source = Source::DockerHub(PullPolicy::Never);
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

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
        let source = Source::DockerHub(PullPolicy::IfNotPresent);
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

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

        let equal = match image.source {
            None => true,
            Some(_) => false,
        };
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

        let addr = "this_is_an_address".to_string();
        let remote = Remote::new(&addr, PullPolicy::Always);
        let image = image.source(Source::Remote(remote));

        let equal = match &image.source {
            Some(r) => match r {
                Source::Remote(r) => {
                    assert_eq!(r.address, addr, "wrong address set in remote after change");
                    match r.pull_policy {
                        PullPolicy::Always => true,
                        _ => false,
                    }
                }
                _ => false,
            },
            None => false,
        };

        assert!(
            equal,
            "changing source does not change the image object correctly"
        );

        let new_tag = "this_is_a_test_tag";
        let image = image.tag(&new_tag);
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
        let client = Docker::connect_with_local_defaults().expect("local docker daemon connection");
        let repository = CONCURRENT_IMAGE_ACCESS_QUEUE.access().await;
        let tag = "latest";
        let image = Image::with_repository(*repository)
            .tag(&tag)
            // Source is overridden to be DockerHub, a valid source.
            .source(Source::DockerHub(PullPolicy::Always));

        // Deinve a custom Remove registry that is invalid.
        let invalid_source = Source::Remote(Remote::new(
            &"this_is_not_a_registry".to_string(),
            PullPolicy::Always,
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
