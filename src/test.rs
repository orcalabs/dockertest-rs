use crate::container::Container;
use crate::error::DockerError;
use crate::image::Source;
use crate::image_instance::{ImageInstance, StartPolicy};
use failure::{format_err, Error};
use futures::future::{self, Future};
use futures::sink::Sink;
use futures::stream::Stream;
use futures::sync::mpsc;
use rand::{self, Rng};
use shiplift;
use std::clone::Clone;
use std::collections::HashMap;
use std::panic;
use std::rc::Rc;
use tokio::runtime::current_thread;

/// Represents the docker test environment,
/// and keep track of all containers
/// that should be started.
pub struct DockerTest {
    /// All ImageInstances that have been added
    /// with a strict StartPolicy.
    strict_instances: Vec<ImageInstance>,
    /// All ImageInstances that have been added
    /// with a relaxed StartPolicy.
    relaxed_instances: Vec<ImageInstance>,
    /// The namespace of all started containers,
    /// this is essentially only a prefix on each container.
    /// Used to more easily identify which containers was
    /// started by DockerTest.
    namespace: String,
    /// The docker client to interact with the docker daemon with.
    client: Rc<shiplift::Docker>,
    /// The default pull source to use for all images.
    /// Images with a specified source will override this default.
    default_source: Source,
}

/// Represents all operations that can be performed
/// against all the started containers and
/// docker during a test.
#[derive(Clone)]
pub struct DockerOperations {
    /// Map with all started containers,
    /// the key is the container name.
    containers: HashMap<String, Vec<Container>>,
}

impl DockerOperations {
    /// Returns a handle to the specified container.
    /// If no container_name was specified when creating the ImageInstance, the Image repository
    /// is the default key for the corresponding container.
    pub fn handle<'a>(&'a self, key: &'a str) -> Result<&'a Container, DockerError> {
        let containers = self.containers.get(key);

        match containers {
            None => Err(DockerError::recoverable(format!(
                "container with key: {} not found",
                key
            ))),
            Some(c) => resolve_container_handle_key(c, key),
        }
    }

    /// Indicate that this test failed with the accompanied message.
    pub fn failure(&self, msg: &str) {
        eprintln!("test failure: {}", msg);
        panic!("test failure: {}", msg);
    }
}

impl DockerTest {
    /// Creates a new DockerTest.
    pub fn new() -> DockerTest {
        DockerTest {
            ..Default::default()
        }
    }

    /// Sets the default source for all images.
    /// All images without a specified source will be pulled from the default source.
    /// DockerTest will default to Local if no default source is provided.
    pub fn with_default_source(self, default_source: Source) -> DockerTest {
        DockerTest {
            default_source,
            ..self
        }
    }

    /// Sets the namespace for all containers created by dockerTest.
    /// All container names will be prefixed with this namespace.
    /// DockerTest defaults to the namespace "dockertest-rs".
    pub fn with_namespace<T: ToString>(self, name: T) -> DockerTest {
        DockerTest {
            namespace: name.to_string(),
            ..self
        }
    }

    pub fn run<T>(&self, test: T)
    where
        T: FnOnce(&DockerOperations) -> () + panic::UnwindSafe,
    {
        let rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let containers = self.setup(rt).unwrap_or_else(|e| {
            panic!(format!("failed to setup environment: {}", e));
        });

        let ops = DockerOperations { containers };

        // TODO: don't clone? or is it necessary?
        let ops_clone = ops.clone();

        let res = {
            let ops_wrapper = panic::AssertUnwindSafe(&ops);

            panic::catch_unwind(move || test(&ops_wrapper))
        };

        let rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        self.teardown(rt, ops_clone).unwrap_or_else(|_e| {
            eprintln!("failed to teardown docker environment");
        });

        if res.is_err() {
            ops.failure("see panic above");
        }
    }

    pub fn add_instance(&mut self, instance: ImageInstance) {
        match &instance.start_policy() {
            StartPolicy::Relaxed => self.relaxed_instances.push(instance),
            StartPolicy::Strict => self.strict_instances.push(instance),
        };
    }

    #[cfg(test)]
    pub(crate) fn source(&self) -> &Source {
        &self.default_source
    }

    fn setup(
        &self,
        mut rt: current_thread::Runtime,
    ) -> Result<HashMap<String, Vec<Container>>, Error> {
        self.pull_images(&mut rt)?;

        // The bound of the channel is equal to the number of senders
        // (how many times the sender is cloned) + input argument.
        // We dont need more slots than num_senders, so we pass 0.
        let (sender, receiver) = mpsc::channel(0);

        self.start_relaxed_containers(&sender, &mut rt);

        let strict_containers = self.start_strict_containers(&mut rt)?;

        let all_containers = self.collect_containers(receiver, &mut rt, strict_containers)?;

        Ok(all_containers)
    }

    fn pull_images(&self, rt: &mut current_thread::Runtime) -> Result<(), DockerError> {
        let mut future_vec = Vec::new();

        for instance in self.strict_instances.iter() {
            let client_clone = self.client.clone();
            let fut = instance.image().pull(client_clone, &self.default_source);

            future_vec.push(fut);
        }

        for instance in self.relaxed_instances.iter() {
            let client_clone = self.client.clone();
            let fut = instance.image().pull(client_clone, &self.default_source);

            future_vec.push(fut);
        }

        let pull_fut = future::join_all(future_vec);

        rt.block_on(pull_fut).map(|_| ())
    }

    fn start_relaxed_containers(
        &self,
        sender: &mpsc::Sender<Container>,
        rt: &mut current_thread::Runtime,
    ) {
        // we clone the vector such that users can run multiple
        // dockertest runs without needing to add ImageInstances
        // for each time
        let cloned_vec = self.relaxed_instances.to_vec();
        let iter = cloned_vec.into_iter();

        for instance in iter {
            let suffix = generate_random_string(20);
            let namespaced_instance = instance.configurate_container_name(&self.namespace, &suffix);

            let sender_clone = sender.clone();
            let client_clone = self.client.clone();
            let start_fut = namespaced_instance
                .start(client_clone)
                .map_err(|e| eprintln!("failed to start container: {}", e))
                .and_then(move |c| {
                    sender_clone
                        .send(c)
                        .map_err(|e| eprintln!("failed to send container in channel: {}", e))
                })
                .and_then(|_| Ok(()));

            rt.spawn(start_fut);
        }
    }

    fn start_strict_containers(
        &self,
        rt: &mut current_thread::Runtime,
    ) -> Result<Vec<Container>, Error> {
        // we clone the vector such that users can run multiple
        // dockertest runs without needing to add ImageInstances
        // for each time
        let cloned_vec = self.strict_instances.to_vec();
        let iter = cloned_vec.into_iter();

        let mut strict_containers: Vec<Container> = Vec::new();

        for instance in iter {
            let suffix = generate_random_string(20);
            let namespaced_instance = instance.configurate_container_name(&self.namespace, &suffix);

            let client_clone = self.client.clone();
            let container = rt.block_on(namespaced_instance.start(client_clone))?;
            strict_containers.push(container);
        }

        Ok(strict_containers)
    }

    fn collect_containers(
        &self,
        receiver: mpsc::Receiver<Container>,
        rt: &mut current_thread::Runtime,
        strict_containers: Vec<Container>,
    ) -> Result<HashMap<String, Vec<Container>>, Error> {
        let mut started_containers: HashMap<String, Vec<Container>> = HashMap::new();

        for c in strict_containers.into_iter() {
            println!("strict: {}", c.name());
            started_containers
                .entry(c.handle_key().to_string())
                .or_insert_with(Vec::new)
                .push(c);
        }

        let relaxed_containers = rt.block_on(
            receiver
                .take(self.relaxed_instances.len() as u64)
                .collect()
                .map_err(|_e| format_err!("failed to collect relaxed containers")),
        )?;

        for c in relaxed_containers.into_iter() {
            println!("relaxed: {}", c.name());
            started_containers
                .entry(c.handle_key().to_string())
                .or_insert_with(Vec::new)
                .push(c);
        }

        Ok(started_containers)
    }

    fn teardown(
        &self,
        mut rt: current_thread::Runtime,
        ops: DockerOperations,
    ) -> Result<(), DockerError> {
        let mut future_vec = Vec::new();

        for (_name, containers) in ops.containers {
            for c in containers {
                future_vec.push(
                    c.remove()
                        .map_err(|e| eprintln!("failed to cleanup container instance: {}", e)),
                );
            }
        }
        let teardown_fut = future::join_all(future_vec);

        rt.block_on(teardown_fut)
            .map(|_| ())
            .map_err(|e| DockerError::teardown(format!("failed to run teardown routines: {:?}", e)))
    }
}

impl Default for DockerTest {
    fn default() -> DockerTest {
        DockerTest {
            default_source: Source::Local,
            strict_instances: Vec::new(),
            relaxed_instances: Vec::new(),
            namespace: "dockertest-rs".to_string(),
            client: Rc::new(shiplift::Docker::new()),
        }
    }
}

// If there only exists one container for the given key we can safely return it.
// If there exists more than one container for the given key, we cannot resolve the key
// as we do not know which container to return.
// This occurs if multiple ImageInstances are added with the same Image repository, and
// without providing a containter_name.
// If the the provided key has been resolved, but there exists no containers in the value
// something has gone wrong internally, it should never occur.
fn resolve_container_handle_key<'a>(
    containers: &'a Vec<Container>,
    key: &'a str,
) -> Result<&'a Container, DockerError> {
    let number_of_containers = containers.len();
    if number_of_containers == 1 {
        containers.first().ok_or(DockerError::recoverable(format!(
            "could not retrieve container with key {}, internal unwrap error",
            key
        )))
    } else if number_of_containers > 1 {
        Err(DockerError::recoverable(
            format!("could not resolve container key: {}, as there are multiple containers with the same key", key)))
    } else if number_of_containers == 0 {
        Err(DockerError::recoverable(format!(
            "could not resolve container key: {}, container did not exist",
            key
        )))
    } else {
        Err(DockerError::recoverable(format!(
            "could not resolve container key: {}, container key existed, but corresponding container was not found",
            key
        )))
    }
}

fn generate_random_string(len: i32) -> String {
    let mut random_string = String::new();
    let mut rng = rand::thread_rng();
    for _i in 0..len {
        let letter: char = rng.gen_range(b'A', b'Z') as char;
        random_string.push(letter);
    }

    random_string
}

#[cfg(test)]
mod tests {
    use crate::container::Container;
    use crate::image::{Image, PullPolicy, Source};
    use crate::image_instance::{ImageInstance, StartPolicy};
    use crate::test::{resolve_container_handle_key, DockerOperations};
    use crate::test_utils;
    use crate::DockerTest;
    use futures::future::Future;
    use futures::stream::Stream;
    use futures::sync::mpsc;
    use std::rc::Rc;
    use tokio::runtime::current_thread;

    // Tests that the default DockerTest constructor produces a valid instance with the correct values set
    #[test]
    fn test_default_constructor() {
        let test = DockerTest::new();
        assert_eq!(
            test.strict_instances.len(),
            0,
            "should not have any strict instances after creation"
        );

        assert_eq!(
            test.relaxed_instances.len(),
            0,
            "should not have any relaxed instances after creation"
        );

        assert_eq!(
            test.namespace,
            "dockertest-rs".to_string(),
            "default namespace was not set correctly"
        );

        let equal = match *test.source() {
            Source::Local => true,
            _ => false,
        };

        assert!(equal, "source not set to local by default");
    }

    // Tests that the with_namespace builder method sets the namespace correctly
    #[test]
    fn test_with_namespace() {
        let namespace = "this_is_a_test_namespace".to_string();
        let test = DockerTest::new().with_namespace(&namespace);

        assert_eq!(
            test.namespace, namespace,
            "default namespace was not set correctly"
        );
    }

    // Tests that the with_default_source builder method sets the default_source_correctly
    #[test]
    fn test_with_default_source() {
        let test = DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::Always));

        let equal = match test.default_source {
            Source::DockerHub(p) => match p {
                PullPolicy::Always => true,
                _ => false,
            },
            _ => false,
        };

        assert!(equal, "default_source was not set correctly");
    }

    // Tests that the setup method returns Container representations for both strict and relaxed
    // startup policies
    #[test]
    fn test_setup() {
        let rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let image_name = "hello-world".to_string();

        let mut test =
            DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::IfNotPresent));

        let image_instance = ImageInstance::with_repository(&image_name);
        let image_instance2 =
            ImageInstance::with_repository(&image_name).with_start_policy(StartPolicy::Strict);

        test.add_instance(image_instance);
        test.add_instance(image_instance2);

        let output = test.setup(rt).expect("failed to perform setup");
        let container_vec = output
            .get(&image_name)
            .expect("failed to retrieve containers with repository as handle key");
        assert_eq!(
            container_vec.len(),
            2,
            "setup did not return a map containing containers for all image instances"
        );

        let mut rt2 = current_thread::Runtime::new().expect("failed to start tokio runtime");

        for (_, containers) in output {
            for container in containers {
                let response = rt2.block_on(test_utils::is_container_running(
                    container.id().to_string(),
                    &client,
                ));
                assert!(
                    response.is_ok(),
                    format!(
                        "failed to check for container liveness: {}",
                        response.unwrap_err()
                    )
                );

                let is_running = response.expect("failed to unwrap liveness probe");
                assert!(
                    is_running,
                    "container should be running after setup is complete"
                );
            }
        }
    }

    // Tests that the pull_images method pulls images with strict startup policy
    #[test]
    fn test_pull_images_strict() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let mut test =
            DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::IfNotPresent));

        let repository = "hello-world".to_string();
        let tag = "latest".to_string();

        let image = Image::with_repository(&repository);
        let image_instance =
            ImageInstance::with_image(image).with_start_policy(StartPolicy::Strict);

        test.add_instance(image_instance);

        let res = rt.block_on(test_utils::delete_image_if_present(
            &repository,
            &tag,
            &client,
        ));
        assert!(
            res.is_ok(),
            format!("failed to delete existing image {}", res.unwrap_err())
        );

        let output = test.pull_images(&mut rt);
        assert!(output.is_ok(), format!("{}", output.unwrap_err()));

        let exists = rt.block_on(test_utils::image_exists_locally(&repository, &tag, &client));
        assert!(
            exists.is_ok(),
            format!(
                "image should exists locally after pulling: {}",
                exists.unwrap_err()
            )
        );
    }

    // Tests that the pull_images method pulls images with strict startup policy
    #[test]
    fn test_pull_images_relaxed() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let mut test =
            DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::IfNotPresent));

        let repository = "hello-world".to_string();
        let tag = "latest".to_string();

        let image = Image::with_repository(&repository);
        let image_instance =
            ImageInstance::with_image(image).with_start_policy(StartPolicy::Relaxed);

        test.add_instance(image_instance);

        let res = rt.block_on(test_utils::delete_image_if_present(
            &repository,
            &tag,
            &client,
        ));
        assert!(
            res.is_ok(),
            format!("failed to delete existing image {}", res.unwrap_err())
        );

        let output = test.pull_images(&mut rt);
        assert!(output.is_ok(), format!("{}", output.unwrap_err()));

        let exists = rt.block_on(test_utils::image_exists_locally(&repository, &tag, &client));
        assert!(
            exists.is_ok(),
            format!(
                "image should exists locally after pulling: {}",
                exists.unwrap_err()
            )
        );
    }

    // Tests that the start_relaxed_containers method starts all the relaxed containers
    #[test]
    fn test_start_relaxed_containers() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let mut test =
            DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::IfNotPresent));

        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

        let res = rt.block_on(image.pull(client.clone(), test.source()));
        assert!(
            res.is_ok(),
            "failed to pull relaxed container image: {}",
            res.unwrap_err()
        );

        let image_id = image.retrieved_id();

        let image_instance =
            ImageInstance::with_image(image).with_start_policy(StartPolicy::Relaxed);

        test.add_instance(image_instance);

        let res = rt.block_on(test_utils::remove_containers(image_id, &client));
        assert!(
            res.is_ok(),
            format!("failed to remove existing containers {}", res.unwrap_err())
        );

        let (sender, receiver) = mpsc::channel(0);

        test.start_relaxed_containers(&sender, &mut rt);

        let res = rt.block_on(
            receiver
                .take(1)
                .collect()
                .map_err(|e| format!("failed to retrieve started relaxed container: {:?}", e)),
        );

        assert!(res.is_ok(), format!("failed to start relaxed container"));
        let mut containers = res.expect("failed to unwrap relaxed container");
        assert_eq!(1, containers.len(), "should only start 1 relaxed container");

        let container = containers
            .pop()
            .expect("failed to unwrap relaxed container");

        let res = rt.block_on(test_utils::is_container_running(
            container.id().to_string(),
            &client,
        ));
        assert!(
            res.is_ok(),
            format!(
                "failed to check liveness of relaxed container: {}",
                res.unwrap_err()
            )
        );
        let is_running = res.expect("failed to unwrap liveness check");

        assert!(
            is_running,
            "relaxed container should be running after starting it"
        );
    }

    // Tests that the start_strict_containers method starts all the strict containers
    #[test]
    fn test_start_strict_containers() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let mut test =
            DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::IfNotPresent));

        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

        let res = rt.block_on(image.pull(client.clone(), test.source()));
        assert!(
            res.is_ok(),
            "failed to pull relaxed container image: {}",
            res.unwrap_err()
        );

        let image_id = image.retrieved_id();

        let image_instance =
            ImageInstance::with_image(image).with_start_policy(StartPolicy::Strict);

        test.add_instance(image_instance);

        let res = rt.block_on(test_utils::remove_containers(image_id, &client));
        assert!(
            res.is_ok(),
            format!("failed to remove existing containers {}", res.unwrap_err())
        );

        let res = test.start_strict_containers(&mut rt);

        assert!(res.is_ok(), format!("failed to start relaxed container"));
        let mut containers = res.expect("failed to unwrap relaxed container");
        assert_eq!(1, containers.len(), "should only start 1 relaxed container");

        let container = containers
            .pop()
            .expect("failed to unwrap relaxed container");

        let res = rt.block_on(test_utils::is_container_running(
            container.id().to_string(),
            &client,
        ));
        assert!(
            res.is_ok(),
            format!(
                "failed to check liveness of relaxed container: {}",
                res.unwrap_err()
            )
        );
        let is_running = res.expect("failed to unwrap liveness check");

        assert!(
            is_running,
            "relaxed container should be running after starting it"
        );
    }

    // Tests that the collect_containers method returns both strict and relaxed containers
    #[test]
    fn test_collect_containers() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let mut test =
            DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::IfNotPresent));

        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

        let res = rt.block_on(image.pull(client.clone(), test.source()));
        assert!(
            res.is_ok(),
            "failed to pull relaxed container image: {}",
            res.unwrap_err()
        );

        let image_instance =
            ImageInstance::with_image(image).with_start_policy(StartPolicy::Relaxed);

        let image2 = Image::with_repository(&repository);

        let res = rt.block_on(image2.pull(client.clone(), test.source()));
        assert!(
            res.is_ok(),
            "failed to pull relaxed container image: {}",
            res.unwrap_err()
        );

        let image_id = image2.retrieved_id();

        let image_instance2 =
            ImageInstance::with_image(image2).with_start_policy(StartPolicy::Strict);

        test.add_instance(image_instance);
        test.add_instance(image_instance2);

        let res = rt.block_on(test_utils::remove_containers(image_id, &client));
        assert!(
            res.is_ok(),
            format!("failed to remove existing containers {}", res.unwrap_err())
        );

        let strict_containers = test
            .start_strict_containers(&mut rt)
            .expect("failed to start strict containers");

        let (sender, receiver) = mpsc::channel(0);

        test.start_relaxed_containers(&sender, &mut rt);

        let mut strict_containers_copy = strict_containers.clone();

        let res = test.collect_containers(receiver, &mut rt, strict_containers);
        assert!(res.is_ok(), "failed to collect containers");

        let containers = res.expect("failed to unwrap collected containers");
        let container_vec = containers
            .get(&repository)
            .expect("failed to retrieve containers with repository as handle key");
        assert_eq!(
            container_vec.len(),
            2,
            "should only exist 2 containers, 1 strict, 1 relaxed"
        );

        let strict_container = strict_containers_copy
            .pop()
            .expect("failed to pop strict container vector");

        let strict_container_name = strict_container.name();

        let c1 = container_vec
            .iter()
            .find(|c| c.name() == strict_container_name);
        assert!(
            c1.is_some(),
            "did not find strict container in collected containers"
        );

        // FIXME: Assert that the output contains the relaxed container aswell.
        // As of now, there is no way to retrieve the container name of the
        // relaxed container since the start_relaxed_container method returns
        // the container through a channel.
        // And that channel is used by the collect containers method, and
        // there can only be one receiver.
    }

    // Tests that the teardown method removes all
    // containers
    #[test]
    fn test_teardown_with_exited_containers() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let mut test =
            DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::IfNotPresent));

        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

        let res = rt.block_on(image.pull(client.clone(), test.source()));
        assert!(
            res.is_ok(),
            "failed to pull relaxed container image: {}",
            res.unwrap_err()
        );

        let image_instance =
            ImageInstance::with_image(image).with_start_policy(StartPolicy::Relaxed);

        test.add_instance(image_instance);

        let res = test.setup(rt);
        assert!(res.is_ok(), "failed to perform setup");

        let containers = res.expect("failed to unwrap containers");

        let containers_copy = containers.clone();

        let ops = DockerOperations {
            containers: containers_copy,
        };

        let rt2 = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let res = test.teardown(rt2, ops);
        assert!(
            res.is_ok(),
            "failed to teardown environment: {}",
            res.unwrap_err()
        );

        let mut rt3 = current_thread::Runtime::new().expect("failed to start tokio runtime");
        for (_, container_vec) in containers {
            for c in container_vec {
                let is_running = rt3
                    .block_on(test_utils::is_container_running(
                        c.id().to_string(),
                        &client,
                    ))
                    .expect("failed to check for container liveness");
                assert!(
                    !is_running,
                    "container should not be running after teardown"
                );
            }
        }
    }
}
