use crate::error::DockerError;
use futures::future::{self, Future};
use futures::stream::Stream;
use shiplift::builder::PullOptions;
use std::rc::Rc;

/// Represents a docker image, and describes
/// where its stored (locally or remote).
#[derive(Clone)]
pub struct Image {
    repository: String,
    tag: String,
    source: Source,
    id: Option<String>,
}

/// Represents the source of an Image,
/// which can either be a remote or local.
#[derive(Clone)]
pub enum Source {
    Local,
    Remote(Remote),
}

/// Represents a remote registry,
/// currently only describes by an address.
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
            id: None,
        }
    }

    /// Sets the tag for this image,
    /// default value is latest.
    pub fn tag(self, tag: String) -> Image {
        Image { tag, ..self }
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

    // Returns the id of the image which was obtained
    // after pulling if neccessary.
    pub(crate) fn retrieved_id(&self) -> Option<String> {
        self.id.clone()
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

    /// Pulls the image if neccessary, the pulling decision
    /// is decided by the image's Source and PullPolicy.
    pub(crate) fn pull<'a>(
        &'a self,
        client: Rc<shiplift::Docker>,
    ) -> impl Future<Item = (), Error = DockerError> + 'a {
        client
            .images()
            .get(&format!("{}:{}", self.repository, self.tag))
            .inspect()
            .then(move |res| {
                let exists = res.is_ok();

                match self.should_pull(exists) {
                    Ok(do_pull) => {
                        if do_pull {
                            future::Either::A(self.do_pull(&client))
                        } else {
                            future::Either::B(future::ok(()))
                        }
                    }
                    Err(e) => future::Either::B(future::err(e)),
                }
            })
            .map_err(|e| DockerError::pull(format!("failed to pull image: {}", e)))
            .map(|_| ())
    }

    // Decides wether we should pull the image based on its
    // Source, PullPolicy, and if it exists locally.
    // Will return an error in the following scenarios:
    // - If the image's Source was set to local but does not exists locally.
    // - If the image's Source was set to remote,
    // the image does not exist locally, and the PullPolicy was set to never.
    // - If the image's Source was set to remote, the image does not exists locally, and we
    // fail to pull from the remote.
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

impl Remote {
    /// Creates a new remote with the given address and PullPolicy.
    pub fn new(address: String, pull_policy: PullPolicy) -> Remote {
        Remote {
            address,
            pull_policy,
        }
    }

    // Returns this remote's PullPolicy.
    pub(crate) fn pull_policy(&self) -> &PullPolicy {
        &self.pull_policy
    }
}

#[cfg(test)]
mod tests {
    use crate::image::{Image, PullPolicy, Remote, Source};
    use failure::{format_err, Error};
    use futures::future::{self, Future};
    use shiplift;
    use shiplift::builder::{ContainerListOptions, RmContainerOptions};
    use std::rc::Rc;
    use tokio::runtime::current_thread;

    // Tests the do_pull method of ImageInstance.
    #[test]
    fn test_image_pull() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let remote = Remote::new("".to_string(), PullPolicy::Always);
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository).source(Source::Remote(remote));

        let exists =
            image_exists_local(&mut rt, &repository).expect("failed to query docker deamon");

        if exists {
            delete_image(&mut rt, &repository).expect("failed to delete image");
        }

        let client = Rc::new(shiplift::Docker::new());
        let pull_fut = image.do_pull(&client);

        let res = rt.block_on(pull_fut);
        assert!(res.is_ok(), format!("{}", res.err().unwrap()));

        let exists =
            image_exists_local(&mut rt, &repository).expect("failed to query docker daemon");
        assert!(exists);
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
    fn delete_image(rt: &mut current_thread::Runtime, image_name: &str) -> Result<(), Error> {
        let client = shiplift::Docker::new();

        rt.block_on(remove_containers(&client, image_name))?;

        rt.block_on(
            client
                .images()
                .get(image_name)
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
}
