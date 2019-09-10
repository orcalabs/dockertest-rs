use crate::error::DockerError;
use futures::future::{self, Future};
use futures::stream::Stream;
use shiplift::builder::PullOptions;
use std::rc::Rc;
use std::sync::RwLock;

/// Represents a docker image, and describes
/// where its stored (locally or remote).
#[derive(Clone)]
pub struct Image {
    repository: String,
    tag: String,
    source: Source,
    id: Rc<RwLock<String>>,
}

/// Represents the source of an Image,
/// which can either be a remote or local.
#[derive(Clone)]
pub enum Source {
    Local,
    Remote(Remote),
}

/// Represents a remote registry,
/// currently only described by an address.
// TODO: Add possibility for credentials, either here
// or in another abstraction
#[derive(Clone)]
pub struct Remote {
    address: String,
    pull_policy: PullPolicy,
}

/// The policy for pulling from remote locations.
#[derive(Clone)]
pub enum PullPolicy {
    Never,
    Always,
    IfNotPresent,
}

impl Image {
    /// Creates an Image with the given repository, will default
    /// to the latest tag and local source.
    pub fn with_repository<T: ToString>(repository: &T) -> Image {
        Image {
            repository: repository.to_string(),
            tag: "latest".to_string(),
            source: Source::Local,
            id: Rc::new(RwLock::new("".to_string())),
        }
    }

    /// Sets the tag for this image,
    /// default value is latest.
    pub fn tag<T: ToString>(self, tag: &T) -> Image {
        Image {
            tag: tag.to_string(),
            ..self
        }
    }

    /// Sets the source for this image,
    /// default value is local.
    pub fn source(self, source: Source) -> Image {
        Image { source, ..self }
    }

    // Returns the repository
    pub(crate) fn repository(&self) -> &str {
        &self.repository
    }

    // Returns the id of the image
    pub(crate) fn retrieved_id(&self) -> String {
        let id = self.id.read().expect("failed to get id lock");
        id.clone()
    }

    // Pulls the image from its source with the given
    // docker client.
    fn do_pull<'a>(
        &'a self,
        client: &Rc<shiplift::Docker>,
    ) -> impl Future<Item = (), Error = DockerError> + 'a {
        let pull_options = PullOptions::builder()
            .image(self.repository.clone())
            .repo(self.repository.clone())
            .tag(self.tag.clone())
            .build();

        shiplift::Images::new(&client)
            .pull(&pull_options)
            .collect()
            .map(|_| ())
            .map_err(move |e| {
                DockerError::pull(format!(
                    "failed to pull image: {}:{}, reason: {}",
                    self.repository, self.tag, e,
                ))
            })
    }

    // Retrieves the id of the image from the local docker daemon and
    // sets that id field in image to that value.
    // If this method is invoked and the image does not exist locally,
    // it will return an error.
    fn retrieve_and_set_id<'a>(
        &'a self,
        client: Rc<shiplift::Docker>,
    ) -> impl Future<Item = (), Error = DockerError> + 'a {
        client
            .images()
            .get(&format!("{}:{}", self.repository, self.tag))
            .inspect()
            .and_then(move |details| {
                let mut id = self.id.write().expect("failed to get id lock");
                *id = details.id.clone();
                future::ok(())
            })
            .map_err(|e| DockerError::pull(format!("failed to retrieve id of image: {}", e)))
    }

    // Checks wether the image exists locally, will return false
    // if it does not exists, but will also return false if we
    // fail to contact the docker daemon.
    // TODO: Return error if docker daemon is unavailable.
    fn does_image_exist<'a>(
        &'a self,
        client: Rc<shiplift::Docker>,
    ) -> impl Future<Item = bool, Error = DockerError> + 'a {
        client
            .images()
            .get(&format!("{}:{}", self.repository, self.tag))
            .inspect()
            .then(|res| future::ok(res.is_ok()))
    }

    /// Pulls the image if neccessary, the pulling decision
    /// is decided by the image's Source and PullPolicy.
    pub(crate) fn pull<'a>(
        &'a self,
        client: Rc<shiplift::Docker>,
    ) -> impl Future<Item = (), Error = DockerError> + 'a {
        let client_clone = client.clone();
        self.does_image_exist(client.clone())
            .and_then(move |exists| match self.should_pull(exists) {
                Ok(do_pull) => {
                    if do_pull {
                        future::Either::A(self.do_pull(&client))
                    } else {
                        future::Either::B(future::ok(()))
                    }
                }
                Err(e) => future::Either::B(future::err(e)),
            })
            .map_err(|e| DockerError::pull(format!("failed to pull image: {}", e)))
            .and_then(move |_| self.retrieve_and_set_id(client_clone))
            .map(|_| ())
    }

    // Decides wether we should pull the image based on its
    // Source, PullPolicy, and if it exists locally.
    fn should_pull(&self, exists: bool) -> Result<bool, DockerError> {
        match &self.source {
            Source::Remote(r) => match r.pull_policy() {
                PullPolicy::Never => {
                    if exists {
                        Ok(false)
                    } else {
                        Err(DockerError::pull("image source was set to remote and pull_policy to never, but the provided image does not exists on the local host"))
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
            },
            Source::Local => {
                if exists {
                    Ok(false)
                } else {
                    Err(DockerError::pull("image source was set to local, but the provided image does not exists on the local host"))
                }
            }
        }
    }
}

// TODO: Use the address to contact a remote repository.
// As of now we always default to docker hub.
impl Remote {
    /// Creates a new remote with the given address and PullPolicy.
    pub fn new<T: ToString>(address: &T, pull_policy: PullPolicy) -> Remote {
        Remote {
            address: address.to_string(),
            pull_policy,
        }
    }

    // Returns this remote's PullPolicy.
    pub(crate) fn pull_policy(&self) -> &PullPolicy {
        &self.pull_policy
    }
}

// TODO: Maybe use the docker cli to perform existence checks, deletion,
// and pulling of images instead of using shiplift to not have cyclic
// dependencies in our tests.
// TODO: As of now, we rely on all tests involving pulling/deleting images to
// use different images,
// so if you wanna add a test you need to find a different image.
// This is gonna become hell if we dont figure out a better way to do this
#[cfg(test)]
mod tests {
    use crate::image::{Image, PullPolicy, Remote, Source};
    use failure::{format_err, Error};
    use futures::future::{self, Future};
    use futures::stream::Stream;
    use shiplift;
    use shiplift::builder::{ContainerListOptions, PullOptions, RmContainerOptions};
    use std::rc::Rc;
    use tokio::runtime::current_thread;

    // Tests that our exposed pull method succeeds
    // with a valid local source.
    // A valid local source is just that the image
    // exists locally.
    // Uses the bash image
    #[test]
    fn test_pull_succeeds_with_valid_local_source() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let tag = "latest".to_string();
        let repository = "bash".to_string();
        let image = Image::with_repository(&repository).tag(&tag);

        let exists =
            image_exists_local(&mut rt, &repository).expect("failed to query docker deamon");

        if !exists {
            pull_image(&repository, &tag, &mut rt).expect("failed to pull image");
        }

        let client = Rc::new(shiplift::Docker::new());

        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should not be set before pulling"
        );

        let res = rt.block_on(image.pull(client));
        assert!(
            res.is_ok(),
            format!(
                "should not fail pull process with local source and existing image: {}",
                res.unwrap_err()
            )
        );
        let expected_id = image_id(&repository, &tag, &mut rt).expect("failed to get image_id");

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
    // This test does not pull any images.
    #[test]
    fn test_pull_fails_with_invalid_local_source() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let tag = "latest".to_string();
        let repository = "bash".to_string();
        let image = Image::with_repository(&repository).tag(&tag);

        let exists =
            image_exists_local(&mut rt, &repository).expect("failed to query docker deamon");

        if exists {
            delete_image(&repository, &tag, &mut rt).expect("failed to delete image");
        }

        let client = Rc::new(shiplift::Docker::new());

        let res = rt.block_on(image.pull(client));
        assert!(
            res.is_err(),
            "should fail pull process with local source and non-existing image"
        );
    }

    // Tests that our exposed pull method succeeds
    // with a valid remote source
    // Uses the registry image.
    #[test]
    fn test_pull_succeeds_with_valid_remote_source() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let tag = "latest".to_string();
        let remote = Remote::new(&"".to_string(), PullPolicy::Always);
        let repository = "registry".to_string();
        let image = Image::with_repository(&repository)
            .source(Source::Remote(remote))
            .tag(&tag);

        let exists =
            image_exists_local(&mut rt, &repository).expect("failed to query docker deamon");

        if exists {
            delete_image(&repository, &tag, &mut rt).expect("failed to pull image");
        }

        let client = Rc::new(shiplift::Docker::new());

        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should not be set before pulling"
        );

        let res = rt.block_on(image.pull(client));
        assert!(
            res.is_ok(),
            format!(
                "should not fail pull process with valid remote source: {}",
                res.unwrap_err()
            )
        );
        let expected_id = image_id(&repository, &tag, &mut rt).expect("failed to get image_id");

        assert_eq!(
            image.retrieved_id(),
            expected_id,
            "the id set for image does not equal the expected value"
        );
    }

    // Tests that our exposed pull method fails
    // with an invalid remote source
    // This test pulls no images.
    #[test]
    fn test_pull_fails_with_invalid_remote_source() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let tag = "this_is_not_a_tag".to_string();
        let remote = Remote::new(&"".to_string(), PullPolicy::Always);
        let repository = "non_existing_repo_yepsi_pepsi".to_string();
        let image = Image::with_repository(&repository)
            .source(Source::Remote(remote))
            .tag(&tag);

        let client = Rc::new(shiplift::Docker::new());

        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should not be set before pulling"
        );

        let res = rt.block_on(image.pull(client));
        assert!(
            res.is_err(),
            "should fail pull process with an invalid remote source",
        );
    }

    // Tests that the retrieve_and_set_id method sets the image id
    // Uses the busybox image.
    #[test]
    fn test_set_image_id() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let tag = "latest".to_string();
        let remote = Remote::new(&"".to_string(), PullPolicy::Always);
        let repository = "busybox".to_string();
        let image = Image::with_repository(&repository)
            .source(Source::Remote(remote))
            .tag(&tag);

        assert_eq!(
            image.retrieved_id(),
            "",
            "image id should be empty before pulling"
        );

        let exists =
            image_exists_local(&mut rt, &repository).expect("failed to query docker deamon");

        if !exists {
            pull_image(&repository, &tag, &mut rt).expect("failed to pull image");
        }

        let expected_id = image_id(&repository, &tag, &mut rt).expect("failed to get image_id");
        let client = Rc::new(shiplift::Docker::new());

        let res = rt.block_on(image.retrieve_and_set_id(client));
        assert!(
            res.is_ok(),
            format!("failed to set image id: {}", res.unwrap_err())
        );

        assert_eq!(
            image.retrieved_id(),
            expected_id,
            "the id set for image does not equal the expected value"
        );
    }

    // Tests that we can check if an image exists locally with
    // the does_image_exist method.
    // Uses the alpine image.
    #[test]
    fn test_image_existence() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let tag = "latest".to_string();
        let remote = Remote::new(&"".to_string(), PullPolicy::Always);
        let repository = "alpine".to_string();
        let image = Image::with_repository(&repository)
            .source(Source::Remote(remote))
            .tag(&tag);

        let exists =
            image_exists_local(&mut rt, &repository).expect("failed to query docker deamon");

        if exists {
            delete_image(&repository, &tag, &mut rt).expect("failed to delete image");
        }

        let client = Rc::new(shiplift::Docker::new());

        let client_clone = client.clone();

        let res = rt
            .block_on(image.does_image_exist(client))
            .expect("failed to set image id");
        assert!(
            !res,
            "should return false when image does not exist locally"
        );

        pull_image(&repository, &tag, &mut rt).expect("failed to pull image");

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
    // Uses the hello-world image.
    #[test]
    fn test_pull_remote_image() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let tag = "latest".to_string();
        let remote = Remote::new(&"".to_string(), PullPolicy::Always);
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository)
            .source(Source::Remote(remote))
            .tag(&tag);

        let exists =
            image_exists_local(&mut rt, &repository).expect("failed to query docker deamon");

        if exists {
            delete_image(&repository, &tag, &mut rt).expect("failed to delete image");
        }

        let client = Rc::new(shiplift::Docker::new());
        let pull_fut = image.do_pull(&client);

        let res = rt.block_on(pull_fut);
        assert!(res.is_ok(), format!("{}", res.err().unwrap()));

        let exists =
            image_exists_local(&mut rt, &repository).expect("failed to query docker daemon");
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
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository).source(Source::Local);

        let exists_locally = true;
        let res = image
            .should_pull(exists_locally)
            .expect("returned error with image locally and source set to local");

        assert!(
            !res,
            "should not pull with image already on the local host and source set to local"
        );

        let exists_locally = false;
        let res = image.should_pull(exists_locally);
        assert!(
            res.is_err(),
            "should return an error without image on local host and source set to local"
        );
    }

    // Tests the behaviour of the should_pull method with the
    // source set to remote and pull_policy set to always
    #[test]
    fn test_should_pull_remote_source_always() {
        let remote = Remote::new(&"remote_registry", PullPolicy::Always);
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository).source(Source::Remote(remote));

        let exists_locally = false;
        let res = image.should_pull(exists_locally).expect("should not return an error when source is remote, pull_policy is always, and image does not exist locally");
        assert!(res, "should pull when pull policy is set to always");

        let exists_locally = true;
        let res = image.should_pull(exists_locally).expect("should not return an error when source is remote, pull_policy is always, and image exists locally");
        assert!(res, "should pull when pull policy is set to always");
    }

    // Tests the behaviour of the should_pull method with the
    // source set to remote and pull_policy set to never
    #[test]
    fn test_should_pull_remote_source_never() {
        let remote = Remote::new(&"remote_registry", PullPolicy::Never);
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository).source(Source::Remote(remote));

        let exists_locally = false;
        let res = image.should_pull(exists_locally);
        assert!(res.is_err(), "should return an error when pull policy is set to never and image does not exist locally");

        let exists_locally = true;
        let res = image
            .should_pull(exists_locally)
            .expect("should not return an error with existing image and pull policy set to never");
        assert!(!res, "should not pull with pull_policy set to never");
    }

    // Tests the behaviour of the should_pull method with the
    // source set to remote and pull_policy set to if_not_present
    #[test]
    fn test_should_pull_remote_source_if_not_present() {
        let remote = Remote::new(&"remote_registry", PullPolicy::IfNotPresent);
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository).source(Source::Remote(remote));

        let exists_locally = true;
        let res = image.should_pull(exists_locally).expect(
            "should not return an error with IfNotPresent pull policy and image existing locally",
        );
        assert!(!res, "should not pull when the image already exists");

        let exists_locally = false;
        let res = image.should_pull(exists_locally).expect(
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
            Source::Local => true,
            _ => false,
        };
        assert!(equal, "should have source set to local as default");
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
            Source::Remote(r) => {
                assert_eq!(r.address, addr, "wrong address set in remote after change");
                match r.pull_policy {
                    PullPolicy::Always => true,
                    _ => false,
                }
            }
            _ => false,
        };

        assert!(
            equal,
            "changing source does not change the image object correctly"
        );

        let new_tag = "this_is_a_test_tag";
        let image = image.tag(&new_tag);
        assert_eq!(image.tag, new_tag, "changing tag does not change image tag");
    }

    // Helper functions that checks wether the given image name exists on the local system
    fn image_exists_local(
        rt: &mut current_thread::Runtime,
        image_name: &str,
    ) -> Result<bool, Error> {
        let client = shiplift::Docker::new();
        let exist_fut = client
            .images()
            .get(&image_name)
            .inspect()
            .then(|res| match res {
                Ok(_) => Ok(true),
                Err(_) => Ok(false),
            })
            .map(|res| res);

        rt.block_on(exist_fut)
    }

    // Helper function that removes a given image from
    // the local system, useful when verifying image
    // pull mechanisms
    fn delete_image(
        repository: &str,
        tag: &str,
        rt: &mut current_thread::Runtime,
    ) -> Result<(), Error> {
        let client = shiplift::Docker::new();

        let image_name = format!("{}:{}", repository, tag);
        rt.block_on(remove_containers(&client, &image_name))?;

        rt.block_on(
            client
                .images()
                .get(&image_name)
                .delete()
                .map_err(|e| format_err!("failed to delete existing image: {}", e))
                .map(|_| ()),
        )
    }
    // Helper function that removes all containers that
    // stems from a given image
    fn remove_containers<'a>(
        client: &'a shiplift::Docker,
        image_name: &'a str,
    ) -> impl Future<Item = (), Error = Error> + 'a {
        client
            .containers()
            .list(&ContainerListOptions::builder().all().build())
            .and_then(move |containers| {
                let mut fut_vec = Vec::new();
                for c in containers {
                    if c.image == image_name {
                        let container_interface = shiplift::Container::new(&client, c.id);
                        let opts = RmContainerOptions::builder().force(true).build();
                        fut_vec.push(container_interface.remove(opts));
                    }
                }

                future::join_all(fut_vec).map(|_| ())
            })
            .map_err(|e| format_err!("failed to remove containers: {}", e))
    }

    // Helper function that pulls a given image with the given tag
    fn pull_image(
        repository: &str,
        tag: &str,
        rt: &mut current_thread::Runtime,
    ) -> Result<(), Error> {
        let client = shiplift::Docker::new();
        let images = shiplift::Images::new(&client);
        let opts = PullOptions::builder().image(repository).tag(tag).build();

        rt.block_on(images.pull(&opts).collect())
            .map(|_| ())
            .map_err(|e| format_err!("failed to pull image: {}", e))
    }

    // Helper function that retreives the id of a given image
    fn image_id(
        repository: &str,
        tag: &str,
        rt: &mut current_thread::Runtime,
    ) -> Result<String, Error> {
        let client = shiplift::Docker::new();
        let id_fut = client
            .images()
            .get(&format!("{}:{}", repository, tag))
            .inspect()
            .map(|res| res.id)
            .map_err(|e| format_err!("failed to get image id {}", e));

        rt.block_on(id_fut)
    }
}
