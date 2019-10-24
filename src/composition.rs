//! Represent a concrete instance of an Image, before it is ran as a Container.

use crate::container::Container;
use crate::error::{DockerError, DockerErrorKind};
use crate::image::Image;
use crate::wait_for::{NoWait, WaitFor};
use futures;
use futures::future::{self, Future};
use shiplift;
use shiplift::builder::{ContainerOptions, RmContainerOptions};
use std::collections::HashMap;
use std::rc::Rc;

/// Specifies the starting policy of a Composition.
/// A Strict policy will enforce that the Composition is started in the order
/// it was added to DockerTest. A Relaxed policy will not enforce any ordering,
/// all Compositions with a Relaxed policy will be started concurrently.
#[derive(Clone)]
pub enum StartPolicy {
    /// Concurrently start the Container with other Relaxed instances.
    Relaxed,
    /// Start Containers' sequentially in the order added to DockerTest.
    Strict,
}

/// Represents an instance of an image.
#[derive(Clone)]
pub struct Composition {
    /// User provided name of the container.
    /// This will dictate the final container_name and the container_handle_key of the container
    /// that will be created from this Composition.
    user_provided_container_name: Option<String>,

    /// The name of the container to be created by this Composition.
    /// At Composition creation this field defaults to the repository name of the associated image.
    /// When adding Compositions to DockerTest, the container_name will be transformed to the following:
    ///     namespace_of_dockerTest - repository_name - random_generated_suffix
    /// If the user have provided a user_provided_container_name, the container_name will look like
    /// the following:
    ///     namespace_of_dockerTest - user_provided_container_name - random_generated_suffix
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

    /// The StartPolicy of this Composition,
    /// defaults to relaxed.
    start_policy: StartPolicy,

    /// The image that this Composition
    /// stems from.
    image: Image,
}

impl Composition {
    /// Creates an image instance with the given repository.
    /// Will create an Image with the same repository.
    pub fn with_repository<T: ToString>(repository: T) -> Composition {
        let copy = repository.to_string();
        Composition {
            user_provided_container_name: None,
            image: Image::with_repository(&copy),
            container_name: copy,
            wait: Rc::new(NoWait {}),
            env: HashMap::new(),
            cmd: Vec::new(),
            start_policy: StartPolicy::Relaxed,
        }
    }

    /// Creates a Composition with the given instance.
    pub fn with_image(image: Image) -> Composition {
        Composition {
            user_provided_container_name: None,
            container_name: image.repository().to_string(),
            image,
            wait: Rc::new(NoWait {}),
            env: HashMap::new(),
            cmd: Vec::new(),
            start_policy: StartPolicy::Relaxed,
        }
    }

    /// Sets the start_policy for this Composition,
    /// defaults to a relaxed policy.
    pub fn with_start_policy(self, start_policy: StartPolicy) -> Composition {
        Composition {
            start_policy,
            ..self
        }
    }

    /// Sets environmental values for the container.
    /// Each key in the map should be a environmental variable,
    /// and its corresponding value will be set as its value.
    pub fn with_env(self, env: HashMap<String, String>) -> Composition {
        Composition { env, ..self }
    }

    /// Sets the command of the container, if not set
    /// the container will default to the image's command, if any.
    pub fn with_cmd(self, cmd: Vec<String>) -> Composition {
        Composition { cmd, ..self }
    }

    /// Sets the name of the container that will eventually be started.
    /// The container name defaults to the repository name.
    pub fn with_container_name<T: ToString>(self, container_name: T) -> Composition {
        Composition {
            user_provided_container_name: Some(container_name.to_string()),
            ..self
        }
    }

    /// Sets the given environment variable to given value.
    /// Note, if with_env is called after a call to env, all values
    /// added by env will be overwritten.
    pub fn env<T: ToString, S: ToString>(&mut self, name: T, value: S) -> &mut Composition {
        self.env.insert(name.to_string(), value.to_string());
        self
    }

    /// Adds the given command.
    /// Note, if with_cmd is called after a call to cmd,
    /// all commands added with cmd will be overwritten.
    pub fn cmd<T: ToString>(&mut self, cmd: T) -> &mut Composition {
        self.cmd.push(cmd.to_string());
        self
    }

    /// Sets the wait_for trait object, this object will be
    /// invoked repeatedly when we are waiting for the container to start.
    /// Defaults to waiting for the container to appear as running.
    pub fn wait_for(self, wait: Rc<dyn WaitFor>) -> Composition {
        Composition { wait, ..self }
    }

    // Configurate the container's name with the given namespace as prefix
    // and suffix.
    // We do this to ensure that we do not have overlapping container names
    // and make it clear which containers are run by DockerTest.
    pub(crate) fn configurate_container_name(self, namespace: &str, suffix: &str) -> Composition {
        let name = match &self.user_provided_container_name {
            None => self.image.repository(),
            Some(n) => n,
        };

        // The docker daemon does not like '/' or '\' in container names
        let stripped_name = name.replace("/", "_");

        Composition {
            container_name: format!("{}-{}-{}", namespace, stripped_name, suffix),
            ..self
        }
    }

    // Consumes the Composition, creates the container and returns the Container object if it
    // was succesfully created.
    pub(crate) fn create<'a>(
        self,
        client: Rc<shiplift::Docker>,
    ) -> impl Future<Item = Container, Error = DockerError> + 'a {
        println!("starting container: {}", self.container_name);

        let wait_for_clone = self.wait.clone();
        let start_policy_clone = self.start_policy.clone();

        let container_name_clone = self.container_name.clone();

        let c1 = client.clone();

        let handle = match &self.user_provided_container_name {
            None => self.image.repository().to_string(),
            Some(n) => n.clone(),
        };

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
                let container_options = ContainerOptions::builder(&self.image.retrieved_id())
                    .cmd(cmds)
                    .env(envs)
                    .name(&self.container_name)
                    .build();
                containers
                    .create(&container_options)
                    .map_err(|e| DockerError::daemon(format!("failed to create container: {}", e)))
            })
            .and_then(move |container_info| {
                let container = Container::new(
                    &container_name_clone,
                    &container_info.id,
                    handle,
                    start_policy_clone,
                    wait_for_clone,
                    client.clone(),
                );

                future::ok(container)
            })
    }

    // Returns the Image associated with this Composition.
    pub(crate) fn image(&self) -> &Image {
        &self.image
    }

    // Returns the StartPolicy of this Composition.
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

#[cfg(test)]
mod tests {
    use crate::composition::{remove_container_if_exists, Composition, StartPolicy};
    use crate::error::DockerErrorKind;
    use crate::image::{Image, PullPolicy, Source};
    use std::collections::HashMap;
    use std::rc::Rc;
    use tokio::runtime::current_thread;

    // Tests that the with_repository constructor creates
    // a Composition with the correct values
    #[test]
    fn test_with_repository_constructor() {
        let repository = "this_is_a_repository".to_string();

        let instance = Composition::with_repository(&repository);
        assert_eq!(
            repository,
            instance.image.repository(),
            "repository is not set to the correct value"
        );
        assert_eq!(
            repository, instance.container_name,
            "container_name should default to the repository"
        );
        assert_eq!(
            instance.env.len(),
            0,
            "there should be no environmental variables after constructing a Composition"
        );
        assert_eq!(
            instance.cmd.len(),
            0,
            "there should be no commands after constructing a Composition"
        );

        let equal = match instance.start_policy {
            StartPolicy::Relaxed => true,
            _ => false,
        };
        assert!(equal, "start_policy should default to relaxed");
    }

    // Tests that the with_image constructor creates
    // a Composition with the correct values
    #[test]
    fn test_with_image_constructor() {
        let repository = "this_is_a_repository".to_string();

        let image = Image::with_repository(&repository);

        let instance = Composition::with_image(image);
        assert_eq!(
            repository,
            instance.image.repository(),
            "repository is not set to the correct value"
        );
        assert_eq!(
            repository, instance.container_name,
            "container_name should default to the repository"
        );
        assert_eq!(
            instance.env.len(),
            0,
            "there should be no environmental variables after constructing a Composition"
        );
        assert_eq!(
            instance.cmd.len(),
            0,
            "there should be no commands after constructing a Composition"
        );

        let equal = match instance.start_policy {
            StartPolicy::Relaxed => true,
            _ => false,
        };
        assert!(equal, "start_policy should default to relaxed");
    }

    // Tests all methods that consumes the Composition
    // and mutates a field
    #[test]
    fn test_mutators() {
        let mut env = HashMap::new();

        let env_variable = "GOPATH".to_string();
        let env_value = "/home/kim/unsafe".to_string();

        env.insert(env_variable, env_value);
        let expected_env = env.clone();

        let cmd = "this_is_a_command".to_string();
        let mut cmds = Vec::new();
        cmds.push(cmd);

        let expected_cmds = cmds.clone();

        let repository = "this_is_a_repository".to_string();

        let container_name = "this_is_a_container_name";

        let instance = Composition::with_repository(&repository)
            .with_start_policy(StartPolicy::Strict)
            .with_env(env)
            .with_cmd(cmds)
            .with_container_name(container_name);

        let equal = match instance.start_policy {
            StartPolicy::Strict => true,
            _ => false,
        };

        assert!(equal, "start_policy was not changed after invoking mutator");
        assert_eq!(
            expected_env, instance.env,
            "environmental variables not set correctly"
        );

        assert_eq!(expected_cmds, instance.cmd, "commands not set correctly");

        let correct_container_name = match instance.user_provided_container_name {
            Some(n) => n == container_name,
            None => false,
        };

        assert!(correct_container_name, "container_name not set correctly");
    }

    // Tests that the env method succesfully
    // adds the given environment variable to the Composition
    #[test]
    fn test_add_env() {
        let env_variable = "this_is_an_env_var".to_string();
        let env_value = "this_is_an_env_value".to_string();

        let repository = "this_is_a_repository".to_string();
        let mut instance = Composition::with_repository(&repository);

        instance.env(env_variable.clone(), env_value.clone());

        assert_eq!(
            *instance
                .env
                .get(&env_variable)
                .expect("failed to get value from map that should be there"),
            env_value,
            "environmental variable not added correctly"
        );
    }

    // Tests that the cmd method succesfully
    // adds the given command to the Composition
    #[test]
    fn test_add_cmd() {
        let cmd = "this_is_a_command".to_string();
        let expected_cmd = vec![cmd.clone()];

        let repository = "this_is_a_repository".to_string();
        let mut instance = Composition::with_repository(&repository);

        instance.cmd(cmd);

        assert_eq!(
            instance.cmd, expected_cmd,
            "command value not added correctly"
        );
    }

    // Tests that we fail to create the Container if its associated image has
    // not been pulled yet.
    // If it exists locally, but the pull process has no been invoked
    // its id will be empty.
    #[test]
    fn test_create_with_non_existing_image() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let repository = "this_repo_does_not_exist".to_string();
        let instance = Composition::with_repository(&repository);

        let client = Rc::new(shiplift::Docker::new());
        let res = rt.block_on(instance.create(client));

        assert!(
            res.is_err(),
            "should fail to start a Composition with non-exisiting image"
        );
    }

    // Tests that we can successfully create a Container from a Composition
    // resulting in a Container with correct values.
    #[test]
    fn test_create_with_existing_image() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let repository = "hello-world".to_string();

        let source = Source::DockerHub(PullPolicy::Always);
        let image = Image::with_repository(&repository);
        let instance = Composition::with_image(image);

        let client = Rc::new(shiplift::Docker::new());

        let res = rt.block_on(instance.image().pull(client.clone(), &source));
        assert!(
            res.is_ok(),
            format!("failed to pull image: {}", res.unwrap_err())
        );

        let res = rt.block_on(instance.create(client));
        assert!(
            res.is_ok(),
            format!("failed to start Composition: {}", res.err().unwrap())
        );
    }

    // Tests that we can successfully create a Contaienr from a Composition,
    // even if there exists a container with the same name.
    // The start method should detect that there already
    // exists a container with the same name,
    // remove it, and create ours.
    #[test]
    fn test_create_with_existing_container() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let repository = "hello-world".to_string();

        let container_name = "this_is_a_container".to_string();

        let source = Source::DockerHub(PullPolicy::IfNotPresent);
        let image = Image::with_repository(&repository);
        let mut instance = Composition::with_image(image);
        instance.container_name = container_name.clone();

        let client = Rc::new(shiplift::Docker::new());

        let res = rt.block_on(instance.image().pull(client.clone(), &source));
        assert!(
            res.is_ok(),
            format!("failed to pull image: {}", res.unwrap_err())
        );

        let res = rt.block_on(instance.create(client.clone()));
        assert!(
            res.is_ok(),
            format!("failed to start Composition: {}", res.err().unwrap())
        );

        let source = Source::DockerHub(PullPolicy::IfNotPresent);
        let image = Image::with_repository(&repository);
        let mut instance = Composition::with_image(image);
        instance.container_name = container_name.clone();

        let client = Rc::new(shiplift::Docker::new());

        let res = rt.block_on(instance.image().pull(client.clone(), &source));
        assert!(
            res.is_ok(),
            format!("failed to pull image: {}", res.unwrap_err())
        );

        let res = rt.block_on(instance.create(client));
        assert!(
            res.is_ok(),
            format!("failed to start Composition: {}", res.err().unwrap())
        );
    }

    // Tests that we can remove an existing container.
    #[test]
    fn test_remove_existing_container() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let repository = "hello-world".to_string();

        let source = Source::DockerHub(PullPolicy::IfNotPresent);
        let image = Image::with_repository(&repository);
        let instance = Composition::with_image(image);

        let container_name = instance.container_name.clone();

        let client = Rc::new(shiplift::Docker::new());

        let res = rt.block_on(instance.image().pull(client.clone(), &source));
        assert!(
            res.is_ok(),
            format!("failed to pull image: {}", res.unwrap_err())
        );

        let res = rt.block_on(instance.create(client.clone()));
        assert!(
            res.is_ok(),
            format!("failed to start Composition: {}", res.err().unwrap())
        );

        let res = rt.block_on(remove_container_if_exists(client, container_name));
        assert!(
            res.is_ok(),
            format!("failed to remove existing container: {}", res.unwrap_err())
        );
    }

    // Tests that we fail when trying to remove a non-existing container.
    #[test]
    fn test_remove_non_existing_container() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let res = rt.block_on(remove_container_if_exists(
            client,
            "non_existing_container".to_string(),
        ));

        let res = match res {
            Ok(_) => false,
            Err(e) => match e.kind() {
                DockerErrorKind::Recoverable(_) => true,
                _ => false,
            },
        };
        assert!(res, format!("should fail to remove non-existing container"));
    }

    // Tests that the configurate_container_name method correctly sets the Composition's
    // container_name when the user has not specified a container_name
    #[test]
    fn test_configurate_container_name_without_user_supplied_name() {
        let repository = "hello-world";
        let composition = Composition::with_repository(&repository);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, repository, suffix);

        let new_instance = composition.configurate_container_name(&namespace, suffix);

        assert_eq!(
            new_instance.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }

    // Tests that the configurate_container_name method correctly sets the Composition's
    // container_name when the user has specified a container_name
    #[test]
    fn test_configurate_container_name_with_user_supplied_name() {
        let repository = "hello-world";
        let container_name = "this_is_a_container";
        let composition =
            Composition::with_repository(&repository).with_container_name(container_name);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, container_name, suffix);

        let new_instance = composition.configurate_container_name(&namespace, suffix);

        assert_eq!(
            new_instance.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }

    // Tests that the configurate_container_name method replaces forward slashes with underscore
    // when a user provided name is given.
    // The docker daemon does not like forward slashes in container names.
    #[test]
    fn test_configurate_container_name_with_user_supplied_name_containing_slashes() {
        let repository = "hello-world";
        let container_name = "this/is/a_container";
        let expected_container_name = "this_is_a_container";

        let composition =
            Composition::with_repository(&repository).with_container_name(container_name);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, expected_container_name, suffix);

        let new_instance = composition.configurate_container_name(&namespace, suffix);

        assert_eq!(
            new_instance.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }

    // Tests that the configurate_container_name method replaces forward slashes with underscore
    // when no user provided container name is provided.
    // The docker daemon does not like forward slashes in container names.
    #[test]
    fn test_configurate_container_name_without_user_supplied_name_containing_slashes() {
        let repository = "hello/world";
        let expected_container_name = "hello_world";

        let composition = Composition::with_repository(&repository);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, expected_container_name, suffix);

        let new_instance = composition.configurate_container_name(&namespace, suffix);

        assert_eq!(
            new_instance.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }
}
