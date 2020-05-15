//! An Image persisted in Docker.

use crate::DockerTestError;

use bollard::{image::CreateImageOptions, Docker};
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
#[derive(Clone)]
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
#[derive(Clone)]
pub struct Remote {
    address: String,
    pull_policy: PullPolicy,
}

/// The policy for pulling from remote locations.
#[derive(Clone)]
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
    async fn do_pull(&self, client: &Docker) -> Result<(), DockerTestError> {
        let options = Some(CreateImageOptions::<&str> {
            from_image: &self.repository,
            repo: &self.repository,
            tag: &self.tag,
            ..Default::default()
        });

        // NOTE: create_image for some reason returns a stream of items.
        match client.create_image(options, None, None).next().await {
            Some(_) => {
                event!(Level::DEBUG, "successfully pulled image");
                Ok(())
            }
            None => Err(DockerTestError::Pull(format!(
                "failed to pull image: {}:{}, reason: empty stream result",
                self.repository, self.tag
            ))),
        }
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
            Err(e) => Err(DockerTestError::Pull(format!(
                "failed to retrieve id of image: {}",
                e
            ))),
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
            Err(_) => Ok(false),
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
            Source::Remote(r) => is_valid_pull_policy(exists, r.pull_policy()),
            Source::DockerHub(p) => is_valid_pull_policy(exists, p),
            Source::Local => {
                if exists {
                    Ok(false)
                } else {
                    Err(DockerTestError::Pull("image source was set to local, but the provided image does not exists on the local host".to_string()))
                }
            }
        }
    }
}

fn is_valid_pull_policy(exists: bool, pull_policy: &PullPolicy) -> Result<bool, DockerTestError> {
    match pull_policy {
        PullPolicy::Never => {
            if exists {
                Ok(false)
            } else {
                Err(DockerTestError::Pull("image source was set to remote and pull_policy to never, but the provided image does not exists on the local host".to_string()))
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

// TODO: Maybe use the docker cli to perform existence checks, deletion,
// and pulling of images instead of using shiplift to not have cyclic
// dependencies in our tests.
#[cfg(test)]
mod tests {
    use crate::image::{Image, PullPolicy, Remote, Source};
    use crate::test_utils;
    use shiplift;
    use std::rc::Rc;
    use tokio::runtime::current_thread;

    // Tests that our exposed pull method succeeds
    // with a valid local source.
    // A valid local source is just that the image
    // exists locally.
    #[test]
    fn test_pull_succeeds_with_valid_local_source() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());
        let client_clone = client.clone();

        let tag = "latest".to_string();
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository).tag(&tag);

        let res = rt.block_on(test_utils::pull_if_not_present(&repository, &tag, &client));
        assert!(
            res.is_ok(),
            format!("failed to pull image: {}", res.unwrap_err())
        );

        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should not be set before pulling"
        );

        let res = rt.block_on(image.pull(client, &Source::Local));
        assert!(
            res.is_ok(),
            format!(
                "should not fail pull process with local source and existing image: {}",
                res.unwrap_err()
            )
        );
        let expected_id = rt
            .block_on(test_utils::image_id(&repository, &tag, &client_clone))
            .expect("failed to get image_id");

        assert_eq!(
            image.retrieved_id(),
            expected_id,
            "the id set for image does not equal the expected value"
        );
    }

    // Tests that our exposed pull method fails with an
    // invalid local source.
    // An invalid local source is just that the
    // image does not exist locally.
    #[test]
    fn test_pull_fails_with_invalid_local_source() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let tag = "latest".to_string();
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository).tag(&tag);

        let res = rt.block_on(test_utils::delete_image_if_present(
            &repository,
            &tag,
            &client,
        ));
        assert!(
            res.is_ok(),
            format!("failed to delete image: {}", res.unwrap_err())
        );

        let res = rt.block_on(image.pull(client, &Source::Local));
        assert!(
            res.is_err(),
            "should fail pull process with local source and non-existing image"
        );
    }

    // Tests that our exposed pull method succeeds
    // with a valid remote source
    #[test]
    fn test_pull_succeeds_with_valid_remote_source() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());
        let client_clone = client.clone();

        let tag = "latest".to_string();
        let source = Source::DockerHub(PullPolicy::Always);
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository).tag(&tag);

        let res = rt.block_on(test_utils::delete_image_if_present(
            &repository,
            &tag,
            &client,
        ));
        assert!(
            res.is_ok(),
            format!("failed to delete image: {}", res.unwrap_err())
        );

        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should not be set before pulling"
        );

        let res = rt.block_on(image.pull(client, &source));
        assert!(
            res.is_ok(),
            format!(
                "should not fail pull process with valid remote source: {}",
                res.unwrap_err()
            )
        );
        let expected_id = rt
            .block_on(test_utils::image_id(&repository, &tag, &client_clone))
            .expect("failed to get image_id");

        assert_eq!(
            image.retrieved_id(),
            expected_id,
            "the id set for image does not equal the expected value"
        );
    }

    // Tests that our exposed pull method fails
    // with an invalid remote source
    #[test]
    fn test_pull_fails_with_invalid_remote_source() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let remote = Remote::new(&"".to_string(), PullPolicy::Always);
        let source = Source::Remote(remote);

        let tag = "this_is_not_a_tag".to_string();
        let repository = "non_existing_repo_yepsi_pepsi".to_string();
        let image = Image::with_repository(&repository).tag(&tag);

        let client = Rc::new(shiplift::Docker::new());

        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should not be set before pulling"
        );

        let res = rt.block_on(image.pull(client, &source));
        assert!(
            res.is_err(),
            "should fail pull process with an invalid remote source",
        );
    }

    // Tests that the retrieve_and_set_id method sets the image id
    #[test]
    fn test_set_image_id() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let tag = "latest".to_string();
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository).tag(&tag);

        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should be empty before pulling"
        );

        let image_id = rt.block_on(test_utils::pull_if_not_present(&repository, &tag, &client));
        assert!(
            image_id.is_ok(),
            format!(
                "failed to pull image: {}",
                image_id.err().expect("failed to get error")
            )
        );
        let image_id = image_id.expect("failed to get image id");

        let res = rt.block_on(image.retrieve_and_set_id(client));
        assert!(
            res.is_ok(),
            format!("failed to set image id: {}", res.unwrap_err())
        );

        assert_eq!(
            image.retrieved_id(),
            image_id,
            "the id set for image does not equal the expected value"
        );
    }

    // Tests that we can check if an image exists locally with
    // the does_image_exist method.
    #[test]
    fn test_image_existence() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let tag = "latest".to_string();
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository).tag(&tag);

        let res = rt.block_on(test_utils::delete_image_if_present(
            &repository,
            &tag,
            &client,
        ));
        assert!(
            res.is_ok(),
            format!("failed to delete image: {}", res.unwrap_err())
        );

        let client_clone = client.clone();

        let res = rt
            .block_on(image.does_image_exist(client))
            .expect("failed to set image id");
        assert!(
            !res,
            "should return false when image does not exist locally"
        );

        let res = rt.block_on(test_utils::pull_image(&repository, &tag, &client_clone));
        assert!(
            res.is_ok(),
            format!("failed to pull image: {}", res.unwrap_err())
        );

        let res = rt
            .block_on(image.does_image_exist(client_clone))
            .expect("failed to check image existence");
        assert!(res, "should return true when image exists locally");
    }

    // Tests that the do_pull method fails when pulling
    // a non-exsiting image
    #[test]
    fn test_pull_remote_non_existing_image() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let remote = Remote::new(&"not_an_address", PullPolicy::Always);
        let repository = "non_existing_repo_yepsi_pepsi".to_string();
        let image = Image::with_repository(&repository).source(Source::Remote(remote));

        let client = Rc::new(shiplift::Docker::new());
        let res = rt.block_on(image.do_pull(&client));
        assert!(res.is_err(), "should fail when pulling non-existing image");
    }
    // Tests that the do_pull method succesfully pulls a remote image
    #[test]
    fn test_pull_remote_image() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let tag = "latest".to_string();
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository).tag(&tag);

        let res = rt.block_on(test_utils::delete_image_if_present(
            &repository,
            &tag,
            &client,
        ));
        assert!(
            res.is_ok(),
            format!("failed to delete image: {}", res.unwrap_err())
        );

        let res = rt.block_on(image.do_pull(&client));
        assert!(
            res.is_ok(),
            format!("failed to pull image: {}", res.err().unwrap())
        );

        let res = rt.block_on(test_utils::image_exists_locally(&repository, &tag, &client));
        assert!(
            res.is_ok(),
            "failed to check image existence: {}",
            res.err().expect("failed to get error")
        );
        let exists = res.expect("failed to get existence result");
        assert!(
            exists,
            "image still exists on the local host after removing it"
        );
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
    #[test]
    fn test_image_source_overrides_default_source_in_pull() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let tag = "latest".to_string();
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository)
            .tag(&tag)
            .source(Source::DockerHub(PullPolicy::Always));

        let unvalid_source = Source::Remote(Remote::new(
            &"this_is_not_a_registry".to_string(),
            PullPolicy::Always,
        ));

        let res = rt.block_on(test_utils::delete_image_if_present(
            &repository,
            &tag,
            &client,
        ));
        assert!(
            res.is_ok(),
            format!("failed to delete image: {}", res.unwrap_err())
        );

        let res = rt.block_on(image.pull(client, &unvalid_source));
        assert!(
            res.is_ok(),
            "the invalid source should be overriden by the provided valid source,
            which should result in a successful pull"
        );
    }
}
