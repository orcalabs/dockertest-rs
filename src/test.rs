use crate::container::Container;
use crate::error::DockerError;
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
}

/// Represents all operations that can be performed
/// against all the started containers and
/// docker during a test.
#[derive(Clone)]
pub struct DockerOperations {
    /// Map with all started containers,
    /// the key is the container name.
    containers: HashMap<String, Container>,
}

impl DockerOperations {
    /// Returns a handle to the specified container.
    // TODO: we need to do a mapping between name
    // and the namespaced/suffixed container name
    // we've made.
    pub fn handle(&self, key: &str) -> Option<&Container> {
        self.containers.get(key)
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

    pub fn namespace<T: ToString>(self, name: &T) -> DockerTest {
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
        let suffix = generate_random_string(20);
        let namespaced_instance = instance.configurate_container_name(&self.namespace, &suffix);

        match &namespaced_instance.start_policy() {
            StartPolicy::Relaxed => self.relaxed_instances.push(namespaced_instance),
            StartPolicy::Strict => self.strict_instances.push(namespaced_instance),
        };
    }

    fn setup(&self, mut rt: current_thread::Runtime) -> Result<HashMap<String, Container>, Error> {
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
            let fut = instance.image().pull(client_clone);

            future_vec.push(fut);
        }

        for instance in self.relaxed_instances.iter() {
            let client_clone = self.client.clone();
            let fut = instance.image().pull(client_clone);

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
            let sender_clone = sender.clone();
            let client_clone = self.client.clone();
            let start_fut = instance
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
            let client_clone = self.client.clone();
            let container = rt.block_on(instance.start(client_clone))?;
            strict_containers.push(container);
        }

        Ok(strict_containers)
    }

    fn collect_containers(
        &self,
        receiver: mpsc::Receiver<Container>,
        rt: &mut current_thread::Runtime,
        strict_containers: Vec<Container>,
    ) -> Result<HashMap<String, Container>, Error> {
        let mut started_containers: HashMap<String, Container> = HashMap::new();

        for c in strict_containers.into_iter() {
            println!("strict: {}", c.name());
            started_containers.insert(c.name().to_string(), c);
        }

        let relaxed_containers = rt.block_on(
            receiver
                .take(self.relaxed_instances.len() as u64)
                .collect()
                .map_err(|_e| format_err!("failed to collect relaxed containers")),
        )?;

        for c in relaxed_containers.into_iter() {
            println!("relaxed: {}", c.name());
            started_containers.insert(c.name().to_string(), c);
        }

        Ok(started_containers)
    }

    fn teardown(
        &self,
        mut rt: current_thread::Runtime,
        ops: DockerOperations,
    ) -> Result<(), DockerError> {
        let mut future_vec = Vec::new();

        for (_name, container) in ops.containers {
            future_vec.push(
                container
                    .remove()
                    .map_err(|e| eprintln!("failed to cleanup container instance: {}", e)),
            );
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
            strict_instances: Vec::new(),
            relaxed_instances: Vec::new(),
            namespace: "dockertest-rs".to_string(),
            client: Rc::new(shiplift::Docker::new()),
        }
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
    use crate::image::{Image, PullPolicy, Remote, Source};
    use crate::image_instance::{ImageInstance, StartPolicy};
    use crate::test::DockerOperations;
    use crate::test_utils;
    use crate::DockerTest;
    use futures::future::Future;
    use futures::stream::Stream;
    use futures::sync::mpsc;
    use std::rc::Rc;
    use tokio::runtime::current_thread;

    // Tests that the DockerTest constructor produces a valid
    // instance with the correct values set
    #[test]
    fn test_constructor() {
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

        let namespace = "this_is_a_namespace".to_string();
        let test = test.namespace(&namespace);
        assert_eq!(test.namespace, namespace, "namespace not set correctly");
    }

    // Tests that the setup method returns Container representations for both strict and relaxed
    // startup policies
    #[test]
    fn test_setup() {
        let rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let image_name = "hello-world".to_string();
        let mut test = DockerTest::new();

        let image_instance = ImageInstance::with_repository(&image_name);
        let image_instance2 =
            ImageInstance::with_repository(&image_name).with_start_policy(StartPolicy::Strict);

        test.add_instance(image_instance);
        test.add_instance(image_instance2);

        let output = test.setup(rt).expect("failed to perform setup");
        assert_eq!(
            output.len(),
            2,
            "setup did not return a map containing containers for all image instances"
        );

        let mut rt2 = current_thread::Runtime::new().expect("failed to start tokio runtime");

        for (_, container) in output {
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

    // Tests that the pull_images method pulls images with strict startup policy
    #[test]
    fn test_pull_images_strict() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());

        let repository = "hello-world".to_string();
        let tag = "latest".to_string();
        let mut test = DockerTest::new();

        let source = Source::Remote(Remote::new(&"addr".to_string(), PullPolicy::Always));
        let image = Image::with_repository(&repository).source(source);
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

        let repository = "hello-world".to_string();
        let tag = "latest".to_string();
        let mut test = DockerTest::new();

        let source = Source::Remote(Remote::new(&"addr".to_string(), PullPolicy::Always));
        let image = Image::with_repository(&repository).source(source);
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

        let repository = "hello-world".to_string();
        let mut test = DockerTest::new();

        let source = Source::Remote(Remote::new(&"addr".to_string(), PullPolicy::IfNotPresent));
        let image = Image::with_repository(&repository).source(source);

        let res = rt.block_on(image.pull(client.clone()));
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

        let repository = "hello-world".to_string();
        let mut test = DockerTest::new();

        let source = Source::Remote(Remote::new(&"addr".to_string(), PullPolicy::IfNotPresent));
        let image = Image::with_repository(&repository).source(source);

        let res = rt.block_on(image.pull(client.clone()));
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

        let repository = "hello-world".to_string();
        let mut test = DockerTest::new();

        let source = Source::Remote(Remote::new(&"addr".to_string(), PullPolicy::IfNotPresent));
        let image = Image::with_repository(&repository).source(source);

        let res = rt.block_on(image.pull(client.clone()));
        assert!(
            res.is_ok(),
            "failed to pull relaxed container image: {}",
            res.unwrap_err()
        );

        let image_instance =
            ImageInstance::with_image(image).with_start_policy(StartPolicy::Relaxed);

        let source2 = Source::Remote(Remote::new(&"addr".to_string(), PullPolicy::IfNotPresent));
        let image2 = Image::with_repository(&repository).source(source2);

        let res = rt.block_on(image2.pull(client.clone()));
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
        assert_eq!(
            containers.len(),
            2,
            "should only exist 2 containers, 1 strict, 1 relaxed"
        );

        let strict_container = strict_containers_copy
            .pop()
            .expect("failed to pop strict container vector");

        let strict_container_name = strict_container.name();

        let c1 = containers
            .iter()
            .find(|c| *c.0 == strict_container_name && c.1.name() == strict_container_name);
        assert!(
            c1.is_some(),
            "did not find relaxed container in collected containers"
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

        let repository = "hello-world".to_string();
        let mut test = DockerTest::new();

        let source = Source::Remote(Remote::new(&"addr".to_string(), PullPolicy::IfNotPresent));
        let image = Image::with_repository(&repository).source(source);

        let res = rt.block_on(image.pull(client.clone()));
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
        for (_, c) in containers {
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
