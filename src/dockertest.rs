//! The main library structures.

use crate::container::Container;
use crate::error::DockerError;
use crate::image::Source;
use crate::{Composition, StartPolicy};
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
    /// All Compositions that have been added
    /// with a strict StartPolicy.
    strict_instances: Vec<Composition>,
    /// All Compositions that have been added
    /// with a relaxed StartPolicy.
    relaxed_instances: Vec<Composition>,
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
    /// If no container_name was specified when creating the Composition, the Image repository
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

    /// Execute the test body within the provided function closure.
    /// All Compositions added to the DockerTest has successfully completed their WaitFor clause
    /// once the test body is executed.
    pub fn run<T>(&self, test: T)
    where
        T: FnOnce(&DockerOperations) -> () + panic::UnwindSafe,
    {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let mut containers: HashMap<String, Vec<Container>> = HashMap::new();

        self.create_containers(&mut containers, &mut rt)
            .unwrap_or_else(|e| {
                self.teardown(&mut rt, &containers).unwrap_or_else(|_e| {
                    eprintln!("failed to teardown docker environment");
                });
                panic!(DockerError::startup(format!(
                    "failed to setup environment: {}",
                    e
                )));
            });

        self.start_containers(&containers, &mut rt)
            .unwrap_or_else(|e| {
                panic!(format!("failed to setup environment: {}", e));
            });

        let ops = DockerOperations { containers };
        let ops_clone = ops.clone();

        let res = {
            let ops_wrapper = panic::AssertUnwindSafe(&ops);

            panic::catch_unwind(move || test(&ops_wrapper))
        };

        self.teardown(&mut rt, &ops_clone.containers)
            .unwrap_or_else(|_e| {
                eprintln!("failed to teardown docker environment");
            });

        if res.is_err() {
            ops.failure("see panic above");
        }
    }

    /// Add a Composition to this DockerTest.
    pub fn add_composition(&mut self, instance: Composition) {
        match &instance.start_policy() {
            StartPolicy::Relaxed => self.relaxed_instances.push(instance),
            StartPolicy::Strict => self.strict_instances.push(instance),
        };
    }

    #[cfg(test)]
    pub(crate) fn source(&self) -> &Source {
        &self.default_source
    }

    fn create_containers(
        &self,
        containers: &mut HashMap<String, Vec<Container>>,
        rt: &mut current_thread::Runtime,
    ) -> Result<(), Error> {
        self.pull_images(rt)?;

        let mut all_compositions: Vec<Composition> = Vec::new();

        all_compositions.append(&mut self.strict_instances.to_vec());
        all_compositions.append(&mut self.relaxed_instances.to_vec());

        for instance in all_compositions {
            let suffix = generate_random_string(20);
            let namespaced_instance = instance.configurate_container_name(&self.namespace, &suffix);

            let create_fut = namespaced_instance.create(self.client.clone());
            let container = rt.block_on(create_fut)?;
            containers
                .entry(container.handle_key().to_string())
                .or_insert_with(Vec::new)
                .push(container);
        }

        Ok(())
    }

    fn start_containers(
        &self,
        created_containers: &HashMap<String, Vec<Container>>,
        rt: &mut current_thread::Runtime,
    ) -> Result<(), Error> {
        let relaxed_containers: Vec<&Container> = created_containers
            .values()
            .flatten()
            .filter(|c| match c.start_policy() {
                StartPolicy::Relaxed => true,
                _ => false,
            })
            .collect();

        let strict_containers: Vec<&Container> = created_containers
            .values()
            .flatten()
            .filter(|c| match c.start_policy() {
                StartPolicy::Strict => true,
                _ => false,
            })
            .collect();

        let num_relaxed_containers = relaxed_containers.len() as i32;

        // The bound of the channel is equal to the number of senders
        // (how many times the sender is cloned) + input argument.
        // We dont need more slots than num_senders, so we pass 0.
        let (sender, receiver) = mpsc::channel(0);

        start_relaxed_containers(&sender, rt, relaxed_containers);

        start_strict_containers(strict_containers, rt)?;

        wait_for_relaxed_containers(receiver, rt, num_relaxed_containers)?;

        Ok(())
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

    fn teardown(
        &self,
        rt: &mut current_thread::Runtime,
        all_containers: &HashMap<String, Vec<Container>>,
    ) -> Result<(), DockerError> {
        let mut future_vec = Vec::new();

        for containers in all_containers.values() {
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
// This occurs if multiple Compositions are added with the same Image repository, and
// without providing a containter_name.
// If the the provided key has been resolved, but there exists no containers in the value
// something has gone wrong internally, it should never occur.
fn resolve_container_handle_key<'a>(
    containers: &'a [Container],
    key: &'a str,
) -> Result<&'a Container, DockerError> {
    let number_of_containers = containers.len();
    if number_of_containers == 1 {
        containers.first().ok_or_else(|| {
            DockerError::recoverable(format!(
                "could not retrieve container with key {}, internal unwrap error",
                key
            ))
        })
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

fn start_relaxed_containers(
    sender: &mpsc::Sender<Result<(), Error>>,
    rt: &mut current_thread::Runtime,
    containers: Vec<&Container>,
) {
    for c in containers {
        let sender_clone = sender.clone();
        let start_fut = c
            .start()
            .map_err(|e| format_err!("failed to start container: {}", e))
            .then(move |_| {
                sender_clone
                    .send(Ok(()))
                    .map_err(|e| eprintln!("failed to send container in channel: {}", e))
            })
            .and_then(|_| Ok(()));

        rt.spawn(start_fut);
    }
}

fn start_strict_containers(
    containers: Vec<&Container>,
    rt: &mut current_thread::Runtime,
) -> Result<(), Error> {
    for c in containers {
        rt.block_on(c.start())?;
    }

    Ok(())
}

fn wait_for_relaxed_containers(
    receiver: mpsc::Receiver<Result<(), Error>>,
    rt: &mut current_thread::Runtime,
    num_relaxed_containers: i32,
) -> Result<(), Error> {
    let relaxed_containers = rt.block_on(
        receiver
            .take(num_relaxed_containers as u64)
            .collect()
            .map_err(|_e| format_err!("failed to collect relaxed containers")),
    )?;

    for c in relaxed_containers.into_iter() {
        if c.is_err() {
            return Err(format_err!(
                "failed to start relaxed containers: {:?}",
                c.err().expect("failed to unwrap error")
            ));
        }
    }

    Ok(())
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
    use crate::dockertest::{
        resolve_container_handle_key, start_relaxed_containers, start_strict_containers,
        wait_for_relaxed_containers,
    };
    use crate::image::{Image, PullPolicy, Source};
    use crate::test_utils;
    use crate::wait_for::NoWait;
    use crate::{Composition, DockerTest, StartPolicy};
    use futures::future::Future;
    use futures::sink::Sink;
    use futures::stream::Stream;
    use futures::sync::mpsc;
    use std::collections::HashMap;
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
        let composition = Composition::with_image(image).with_start_policy(StartPolicy::Strict);

        test.add_composition(composition);

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
        let composition = Composition::with_image(image).with_start_policy(StartPolicy::Relaxed);

        test.add_composition(composition);

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

    // Tests that the start_relaxed_containers function starts all the relaxed containers
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

        let composition = Composition::with_image(image).with_start_policy(StartPolicy::Relaxed);

        test.add_composition(composition);

        rt.block_on(test_utils::remove_containers(image_id, &client))
            .expect("failed to remove existing containers");

        let mut containers: HashMap<String, Vec<Container>> = HashMap::new();

        test.create_containers(&mut containers, &mut rt)
            .expect("failed to create containers");

        let created_containers: Vec<&Container> = containers.values().flatten().collect();

        let container_id = created_containers
            .first()
            .expect("failed to get container from vector")
            .id()
            .to_string();

        let (sender, receiver) = mpsc::channel(0);

        start_relaxed_containers(&sender, &mut rt, created_containers);

        let mut res = rt
            .block_on(
                receiver
                    .take(1)
                    .collect()
                    .map_err(|e| format!("failed to retrieve started relaxed container: {:?}", e)),
            )
            .expect("failed to retrieve result of startup procedure");

        let result = res
            .pop()
            .expect("failed to retrieve the first result from vector");

        assert!(result.is_ok(), format!("failed to start relaxed container"));

        let is_running = rt
            .block_on(test_utils::is_container_running(container_id, &client))
            .expect("failed to check liveness of relaxed container");

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

        rt.block_on(image.pull(client.clone(), test.source()))
            .expect("failed to pull relaxed container image");

        let image_id = image.retrieved_id();

        let composition = Composition::with_image(image).with_start_policy(StartPolicy::Strict);

        test.add_composition(composition);

        rt.block_on(test_utils::remove_containers(image_id, &client))
            .expect("failed to remove existing containers");

        let mut containers: HashMap<String, Vec<Container>> = HashMap::new();

        test.create_containers(&mut containers, &mut rt)
            .expect("failed to create containers");

        let created_containers: Vec<&Container> = containers.values().flatten().collect();

        let container_id = created_containers
            .first()
            .expect("failed to get container from vector")
            .id()
            .to_string();

        let res = start_strict_containers(created_containers, &mut rt);

        assert!(res.is_ok(), format!("failed to start relaxed container"));

        let is_running = rt
            .block_on(test_utils::is_container_running(container_id, &client))
            .expect("failed to check liveness of relaxed container");

        assert!(
            is_running,
            "relaxed container should be running after starting it"
        );
    }

    // Tests that the wait_for_relaxed_containers function waits for the results send through its
    // input channel
    #[test]
    fn test_wait_for_relaxed_container() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let (sender, receiver) = mpsc::channel(0);

        let send_fut = sender
            .send(Ok(()))
            .map_err(|e| eprintln!("failed to send empty OK through channel {}", e))
            .map(|_| ());

        rt.spawn(send_fut);

        let res = wait_for_relaxed_containers(receiver, &mut rt, 1);
        assert!(res.is_ok(), "failed to receive results through channel");
    }

    // Tests that the teardown method removes all containers
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

        let composition = Composition::with_image(image).with_start_policy(StartPolicy::Relaxed);

        test.add_composition(composition);

        let mut containers: HashMap<String, Vec<Container>> = HashMap::new();

        test.create_containers(&mut containers, &mut rt)
            .expect("failed to create containers");

        test.start_containers(&containers, &mut rt)
            .expect("failed to start containers");

        let res = test.teardown(&mut rt, &containers);
        assert!(
            res.is_ok(),
            "failed to teardown environment: {}",
            res.unwrap_err()
        );

        for (_, container_vec) in containers {
            for c in container_vec {
                let is_running = rt
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

    // Tests that the resolve_container_handle_key method successfully resolves a handle_key to a
    // container with only one container exisiting for that handle.
    #[test]
    fn test_resolve_container_handle_key_with_one_container() {
        let client = Rc::new(shiplift::Docker::new());
        let name = "this_is_a_name";
        let id = "this_is_a_id";
        let handle = "this_is_a_handle_key";

        let container = Container::new(
            name,
            id,
            handle,
            StartPolicy::Relaxed,
            Rc::new(NoWait {}),
            client,
        );
        let container_id = container.id().to_string();

        let mut containers = Vec::new();
        containers.push(container);

        let resolved_container = resolve_container_handle_key(&containers, handle);

        assert!(
            resolved_container.is_ok(),
            "failed to retrieve container from vector with only one container"
        );

        let output = resolved_container.expect("failed to unwrap container");
        assert_eq!(
            output.id(),
            container_id,
            "resolved container_id does not match original container"
        );
    }

    // Tests that the resolve_container_handle_key method fails to resolve a handle_key to a
    // container when there exists multiple containers with the same handle_key.
    #[test]
    fn test_resolve_container_handle_key_with_two_container() {
        let client = Rc::new(shiplift::Docker::new());
        let name = "this_is_a_name";
        let id = "this_is_a_id";
        let handle = "this_is_a_handle_key";

        let id2 = "this_is_a_id2";

        let container = Container::new(
            name,
            id,
            handle,
            StartPolicy::Relaxed,
            Rc::new(NoWait {}),
            client.clone(),
        );
        let container2 = Container::new(
            name,
            id2,
            handle,
            StartPolicy::Relaxed,
            Rc::new(NoWait {}),
            client,
        );

        let mut containers = Vec::new();
        containers.push(container);
        containers.push(container2);

        let resolved_container = resolve_container_handle_key(&containers, handle);
        assert!(
            resolved_container.is_err(),
            "should not be able to resolve handle_key with multiple containers with same handle key"
        );
    }

    // Tests that the resolve_container_handle_key method fails to resolve a handle_key to a
    // container when there exists no containers for the given handle_key
    #[test]
    fn test_resolve_container_handle_key_with_zero_container() {
        let handle = "this_is_a_handle_key";
        let containers = Vec::new();

        let resolved_container = resolve_container_handle_key(&containers, handle);
        assert!(
            resolved_container.is_err(),
            "should not be able to resolve handle_key with no containers"
        );
    }
}
