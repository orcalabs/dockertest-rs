use crate::container::Container;
use crate::error::{DockerError, DockerErrorKind};
use crate::image::Image;
use crate::wait_for::{DefaultWait, WaitFor};
use futures;
use futures::future::Future;
use shiplift;
use shiplift::builder::{ContainerOptions, RmContainerOptions};
use std::collections::HashMap;
use std::rc::Rc;

/// Specifies the starting policy of an ImageInstance.
/// A Strict policy will enforce that this image is started
/// in the order it was added to DockerTest.
/// A Relaxed policy will not enforce any ordering, all
/// ImageInstance with a Relaxed policy will be started
/// concurrently.
#[derive(Clone)]
pub enum StartPolicy {
    Relaxed,
    Strict,
}

/// Represents an instance of an image.
#[derive(Clone)]
pub struct ImageInstance {
    /// The name of the container to be created by
    /// this image instance.
    /// Defaults to the repository name given
    /// to the ImageInstance.
    container_name: String,

    /// Trait object that is responsible for
    /// waiting for the container that will
    /// be created to be ready for service.
    /// Defaults to waiting for the container
    /// to appear as running.
    wait: Rc<dyn WaitFor>,

    /// The environmentable variables that will be
    /// passed to the container.
    env: HashMap<String, String>,

    /// The command to pass to the container.
    cmd: Vec<String>,

    /// The StartPolicy of this ImageInstance,
    /// defaults to relaxed.
    start_policy: StartPolicy,

    /// The image that this ImageInstance
    /// stems from.
    image: Image,
}

impl ImageInstance {
    /// Creates an image instance with the given repository.
    /// Will create an Image with the same repository.
    pub fn with_repository(repository: &str) -> ImageInstance {
        ImageInstance {
            image: Image::with_repository(&repository),
            container_name: repository.to_string(),
            wait: Rc::new(DefaultWait {}),
            env: HashMap::new(),
            cmd: Vec::new(),
            start_policy: StartPolicy::Relaxed,
        }
    }

    /// Creates an ImageInstance with the given instance.
    pub fn with_image(image: Image) -> ImageInstance {
        ImageInstance {
            container_name: image.repository().to_string(),
            image,
            wait: Rc::new(DefaultWait {}),
            env: HashMap::new(),
            cmd: Vec::new(),
            start_policy: StartPolicy::Relaxed,
        }
    }

    /// Sets the start_policy for this ImageInstance,
    /// defaults to a relaxed policy.
    pub fn with_start_policy(self, start_policy: StartPolicy) -> ImageInstance {
        ImageInstance {
            start_policy,
            ..self
        }
    }

    /// Sets environmental values for the container.
    /// Each key in the map should be a environmental variable,
    /// and its corresponding value will be set as its value.
    pub fn with_env(self, env: HashMap<String, String>) -> ImageInstance {
        ImageInstance { env, ..self }
    }

    /// Sets the command of the container, if not set
    /// the container will default to the image's command, if any.
    pub fn with_cmd(self, cmd: Vec<String>) -> ImageInstance {
        ImageInstance { cmd, ..self }
    }

    /// Sets the name of the container that will eventually be started.
    /// The container name defaults to the repository name.
    pub fn container_name(self, container_name: String) -> ImageInstance {
        ImageInstance {
            container_name,
            ..self
        }
    }

    /// Sets the given environment variable to given value.
    /// Note, if with_env is called after a call to env, all values
    /// added by env will be overwritten.
    pub fn env(&mut self, name: String, value: String) -> &mut ImageInstance {
        self.env.insert(name, value);
        self
    }

    /// Adds the given command.
    /// Note, if with_cmd is called after a call to cmd,
    /// all commands added with cmd will be overwritenn.
    pub fn cmd(&mut self, cmd: String) -> &mut ImageInstance {
        self.cmd.push(cmd);
        self
    }

    /// Sets the wait_for trait object, this object will be
    /// invoked repeatedly when we are waiting for the container to start.
    /// Defaults to waitinf for the container to appear as running.
    pub fn wait_for(self, wait: Rc<dyn WaitFor>) -> ImageInstance {
        ImageInstance { wait, ..self }
    }

    // Configurate the container's name with the given namespace as prefix
    // and suffix.
    // We do this to ensure that we do not have overlapping container names
    // and make it clear which containers are run by DockerTest.
    pub(crate) fn configurate_container_name(self, namespace: &str, suffix: &str) -> ImageInstance {
        ImageInstance {
            container_name: format!("{}-{}-{}", namespace, self.container_name, suffix),
            ..self
        }
    }

    // Consumes the ImageInstance, starts the container, and returns the
    // Container object if it was succesfully started.
    // TODO: wait for it to be ready for service.
    pub(crate) fn start<'a>(
        self,
        client: Rc<shiplift::Docker>,
    ) -> impl Future<Item = Container, Error = DockerError> + 'a {
        println!("starting container: {}", self.container_name);

        // TODO don't clone, do something else?
        // no idea how to have each closure share a
        // client without cloning
        let wait_for_clone = self.wait.clone();

        let container_name_clone = self.container_name.clone();
        let c1 = client.clone();
        let c2 = client.clone();

        let remove_fut = remove_container_if_exists(client.clone(), self.container_name.clone());

        remove_fut
            .then(|res| match res {
                Ok(_) => Ok(()),
                Err(e) => match e.kind() {
                    DockerErrorKind::Recoverable(_) => Ok(()),
                    _ => Err(e),
                },
            })
            .and_then(move |_| {
                // As we can't return temporary values owned by this closure
                // we have to first convert our map into a vector of owned strings,
                // then convert it to a vector of borrowed strings (&str).
                // There is probably a better way to do this...
                let envs: Vec<String> = self
                    .env
                    .iter()
                    .map(|(key, value)| format!("{}={}", key, value))
                    .collect();
                let envs = envs.iter().map(|s| s.as_ref()).collect();
                let cmds = self.cmd.iter().map(|s| s.as_ref()).collect();

                let containers = c1.containers();
                // TODO fixx unwrap
                let container_options =
                    ContainerOptions::builder(&self.image.retrieved_id().unwrap())
                        .cmd(cmds)
                        .env(envs)
                        .name(&self.container_name)
                        .build();
                containers
                    .create(&container_options)
                    .map_err(|e| DockerError::daemon(format!("failed to create container: {}", e)))
            })
            .and_then(move |container_info| {
                let new_container = shiplift::Container::new(&c2, container_info.id);
                let id = new_container.id().to_string();
                new_container
                    .start()
                    .map_err(|e| DockerError::daemon(format!("failed to start container: {}", e)))
                    .map(move |_| id)
            })
            .map(move |container_id| {
                Container::new(
                    container_name_clone,
                    client.clone(),
                    wait_for_clone,
                    container_id,
                )
            })
    }

    // Returns the Image associated with this ImageInstance.
    pub(crate) fn image(&self) -> &Image {
        &self.image
    }

    // Returns the StartPolicy of this ImageInstance.
    pub(crate) fn start_policy(&self) -> &StartPolicy {
        &self.start_policy
    }
}

// Forcefully removes the given container if it exists.
fn remove_container_if_exists(
    client: Rc<shiplift::Docker>,
    name: String,
) -> impl Future<Item = (), Error = DockerError> {
    client
        .containers()
        .get(&name)
        .inspect()
        .map_err(|e| DockerError::recoverable(format!("container did not exist: {}", e)))
        .and_then(move |_| {
            let opts = RmContainerOptions::builder().force(true).build();
            client.containers().get(&name).remove(opts).map_err(|e| {
                DockerError::daemon(format!("failed to remove existing container: {}", e))
            })
        })
        .map(|_| ())
}
