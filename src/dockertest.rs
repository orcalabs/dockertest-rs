//! The main library structures.

use crate::container::{CleanupContainer, PendingContainer, RunningContainer};
use crate::error::DockerError;
use crate::image::Source;
use crate::{Composition, StartPolicy};
use futures::future::{self, Future};
use futures::sink::Sink;
use futures::stream::Stream;
use futures::sync::mpsc;
use rand::{self, Rng};
use shiplift::builder::RmContainerOptions;
use std::clone::Clone;
use std::collections::{HashMap, HashSet};
use std::panic;
use std::rc::Rc;
use tokio::runtime::current_thread;

/// Represents the docker test environment,
/// and keep track of all containers
/// that should be started.
pub struct DockerTest {
    /// All Compositions that have been added to this test run.
    /// They are stored in the order they where added by `add_composition`.
    compositions: Vec<Composition>,
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

impl Default for DockerTest {
    fn default() -> DockerTest {
        DockerTest {
            default_source: Source::Local,
            compositions: Vec::new(),
            namespace: "dockertest-rs".to_string(),
            client: Rc::new(shiplift::Docker::new()),
        }
    }
}

/// Represents all operations that can be performed
/// against all the started containers and
/// docker during a test.
#[derive(Clone)]
pub struct DockerOperations {
    /// Map with all started containers,
    /// the key is the container name.
    containers: Keeper<RunningContainer>,
}

// TODO: Add `containers` method that return a Vec<&RunningContainer>, in the order
// added by `add_composition`?
impl DockerOperations {
    /// Returns a handle to the specified container.
    /// If no container_name was specified when creating the Composition, the Image repository
    /// is the default key for the corresponding container.
    pub fn handle<'a>(&'a self, key: &'a str) -> Result<&'a RunningContainer, DockerError> {
        if self.containers.lookup_collisions.contains(key) {
            return Err(DockerError::recoverable(format!(
                "handle key '{}' defined multiple times",
                key
            )));
        }

        match self.containers.lookup_handlers.get(key) {
            None => Err(DockerError::recoverable(format!(
                "container with key: {} not found",
                key
            ))),
            Some(c) => Ok(&self.containers.kept[*c]),
        }
    }

    /// Indicate that this test failed with the accompanied message.
    pub fn failure(&self, msg: &str) {
        eprintln!("test failure: {}", msg);
        panic!("test failure: {}", msg);
    }
}

/// The purpose of `Keeper<T>` is to preserve a generic way of keeping the
/// handle resolution and storage of *Container objects as they move
/// through the lifecycle of `Composition` -> `PendingContainer` -> `RunningContainer`.
///
#[derive(Clone)]
struct Keeper<T> {
    /// If we have any handle collisions, they are registered here.
    /// Thus, if any reside here, they cannot be dynamically referenced.
    lookup_collisions: HashSet<String>,
    /// This map stores the mapping between a handle and its index into `kept`.
    lookup_handlers: HashMap<String, usize>,
    /// The series of T owned by the Keeper.
    kept: Vec<T>,
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
    pub fn run<T>(mut self, test: T)
    where
        T: FnOnce(&DockerOperations) -> () + panic::UnwindSafe,
    {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        // Resolve all name mappings prior to creation
        // We might want to support refering to a Composition handler name
        // prior to creating the PendingContainer (in the future).
        // It therefore makes sense to split the verification/handling upfront,
        // so it is streamlined with the teardown regardless of when it must be performed.
        let compositions: Keeper<Composition> = self.validate_composition_handlers();

        // Make sure all the images are present on the local docker daemon before we create
        // the containers from them.
        self.pull_images(&mut rt, &compositions)
            .unwrap_or_else(|e| {
                panic!(e);
            });

        // Create PendingContainers from the Compositions
        let pending_containers: Keeper<PendingContainer> = self
            .create_containers(&mut rt, compositions)
            .unwrap_or_else(|e| {
                self.teardown(&mut rt, e);
                panic!(DockerError::startup("failed to setup environment").to_string());
            });

        // Start the PendingContainers
        let running_containers: Keeper<RunningContainer> = self
            .start_containers(&mut rt, pending_containers)
            .unwrap_or_else(|e| {
                self.teardown(&mut rt, e);
                panic!(DockerError::startup("failed to setup environment").to_string());
            });

        // Create the set of cleanup containers used after the test body
        let cleanup_containers = running_containers
            .kept
            .iter()
            .map(|x| CleanupContainer {
                id: x.id().to_string(),
            })
            .collect();

        // We are ready to invoke the test body now
        let ops = DockerOperations {
            containers: running_containers,
        };

        // Run test body
        let res = {
            let ops_wrapper = panic::AssertUnwindSafe(&ops);

            panic::catch_unwind(move || test(&ops_wrapper))
        };

        self.teardown(&mut rt, cleanup_containers);

        if res.is_err() {
            ops.failure("see panic above");
        }
    }

    /// Add a Composition to this DockerTest.
    pub fn add_composition(&mut self, instance: Composition) {
        self.compositions.push(instance);
    }

    #[cfg(test)]
    pub(crate) fn source(&self) -> &Source {
        &self.default_source
    }

    /// Creates the set of `PendingContainer`s from the `Composition`s.
    ///
    /// This function assumes that all images required by the `Composition`s are
    /// present on the local docker daemon.
    fn create_containers(
        &self,
        rt: &mut current_thread::Runtime,
        compositions: Keeper<Composition>,
    ) -> Result<Keeper<PendingContainer>, Vec<CleanupContainer>> {
        // NOTE: The insertion order is preserved.
        let mut pending = vec![];

        for instance in compositions.kept.into_iter() {
            let suffix = generate_random_string(20);
            let namespaced_instance = instance.configure_container_name(&self.namespace, &suffix);

            let create_fut = namespaced_instance.create(self.client.clone());
            match rt.block_on(create_fut) {
                Ok(container) => {
                    pending.push(container);
                }
                Err(_) => {
                    // Error condition arose - we return the successfully created containers
                    // (for cleanup purposes)
                    return Err(pending.into_iter().map(|x| x.into()).collect());
                }
            }
        }

        Ok(Keeper::<PendingContainer> {
            lookup_collisions: compositions.lookup_collisions,
            lookup_handlers: compositions.lookup_handlers,
            kept: pending,
        })
    }

    /// Start all `PendingContainer` we've created.
    ///
    /// On error, a tuple of two vectors is returned - containing those containers
    /// we have successfully started and those not yet started.
    fn start_containers(
        &mut self,
        rt: &mut current_thread::Runtime,
        mut pending_containers: Keeper<PendingContainer>,
    ) -> Result<Keeper<RunningContainer>, Vec<CleanupContainer>> {
        // We have one issue we would like to solve here:
        // Start all pending containers, and retain the ordered indices used
        // for the Keeper::<T> structure, whilst going though the whole transformation
        // from PendingContainer to RunningContainer.
        //
        // We can rely on the fact that the lookup_* variants will be upheld based on
        // handle name, even though we've "lost" it from Composition as of now.
        // However, we only need to make sure that the `kept` vector is ordered
        // in the same fashion as when the original Keeper::<Composition> was constructed.
        // Therefore, we copy the set of ids in the ordered `kept` vector, and at the end
        // right before we return Keeper::<RunningContainer>, we sort the `kept` vector
        // on this same predicate.
        let mut original_ordered_ids = vec![];
        pending_containers
            .kept
            .iter()
            .for_each(|c| original_ordered_ids.push(c.id.to_string()));

        // Replace the `kept` vector into the stack frame
        let pending = std::mem::replace(&mut pending_containers.kept, vec![]);
        let (relaxed, strict): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|c| c.start_policy == StartPolicy::Relaxed);

        // The bound of the channel is equal to the number of senders
        // (how many times the sender is cloned) + input argument.
        // We dont need more slots than num_senders, so we pass 0.
        let (sender, receiver) = mpsc::channel(0);

        // Asynchronously start all relaxed containers.
        // Each completed container will signal back on the mpsc channel.
        let starting_relaxed = start_relaxed_containers(&sender, rt, relaxed);

        let mut cleanup = vec![];
        let mut running_containers = vec![];

        match start_strict_containers(rt, strict) {
            Ok(mut r) => running_containers.append(&mut r),
            Err(mut c) => cleanup.append(&mut c),
        }
        match wait_for_relaxed_containers(rt, receiver, starting_relaxed) {
            Ok(mut r) => running_containers.append(&mut r),
            Err(mut c) => cleanup.append(&mut c),
        }

        if cleanup.is_empty() {
            sort_running_containers_into_insertion_order(
                &mut running_containers,
                original_ordered_ids,
            );
            Ok(Keeper::<RunningContainer> {
                kept: running_containers,
                lookup_collisions: pending_containers.lookup_collisions,
                lookup_handlers: pending_containers.lookup_handlers,
            })
        } else {
            Err(cleanup)
        }
    }

    /// Pull the `Image` of all `Composition`s present in `compositions`.
    ///
    /// This will ensure that all docker images is present on the local daemon
    /// and we are able to issue a create container operation.
    fn pull_images(
        &self,
        rt: &mut current_thread::Runtime,
        compositions: &Keeper<Composition>,
    ) -> Result<(), DockerError> {
        let mut future_vec = Vec::new();

        for composition in compositions.kept.iter() {
            let client_clone = self.client.clone();
            let fut = composition.image().pull(client_clone, &self.default_source);

            future_vec.push(fut);
        }

        let pull_fut = future::join_all(future_vec);

        rt.block_on(pull_fut).map(|_| ())
    }

    /// Forcefully remove the `CleanupContainer` objects from `cleanup`.
    ///
    /// All errors are discarded.
    fn teardown(&self, rt: &mut current_thread::Runtime, cleanup: Vec<CleanupContainer>) {
        // We spawn all cleanup procedures independently, because we want to cleanup
        // as much as possible, even if one fail.
        for c in cleanup {
            let ops = RmContainerOptions::builder().force(true).build();
            rt.spawn(
                shiplift::Container::new(&self.client, c.id)
                    .remove(ops)
                    .map_err(|_| ()),
            );
        }
        rt.run().expect("runtime failure");
    }

    /// Make sure all `Composition`s registered does not conflict.
    // NOTE(clippy): cannot perform the desired operation with suggested action
    #[allow(clippy::map_entry)]
    fn validate_composition_handlers(&mut self) -> Keeper<Composition> {
        // If the user has supplied two compositions with user provided container names
        // that conflict, we have an outright error.
        // TODO: Implement this check

        let mut handlers: HashMap<String, usize> = HashMap::new();
        let mut collisions: HashSet<String> = HashSet::new();

        // Replace the original vec in DockerTest.compositions.
        // - We take ownership of it into Keeper<Composition>.
        // NOTE: The insertion order is preserved.
        let compositions = std::mem::replace(&mut self.compositions, vec![]);
        for (i, composition) in compositions.iter().enumerate() {
            let handle = composition.handle();

            if handlers.contains_key(&handle) {
                // Mark as collision key
                collisions.insert(handle);
            } else {
                handlers.insert(handle, i);
            }
        }

        Keeper::<Composition> {
            lookup_collisions: collisions,
            lookup_handlers: handlers,
            kept: compositions,
        }
    }
}

/// Sort `RunningContainer`s in the order provided by the vector of ids.
///
/// The set of RunningContainers may be collected in any order.
/// To rectify the situation, we must sort the RunningContainers to be ordered
/// according to the order given by the vector of ids.
///
///   X   Y   Z   Q      <--- `RunningContainer`s
/// -----------------
/// | D | C | B | A |    <--- ids of `running`
/// -----------------
///
/// -----------------
/// | D | B | A | C |    <--- ids of `insertion_order_ids`
/// -----------------
///
/// Transform `running` into this:
///
///   X   Z   Q   Y
/// -----------------
/// | D | B | A | C |
/// -----------------
///
fn sort_running_containers_into_insertion_order(
    running: &mut Vec<RunningContainer>,
    insertion_order_ids: Vec<String>,
) {
    running.sort_unstable_by(|a, b| {
        // Compare the two by their index into the original ordering
        // FIXME: unwrap
        let ai = insertion_order_ids
            .iter()
            .position(|i| i == a.id())
            .unwrap();
        let bi = insertion_order_ids
            .iter()
            .position(|i| i == b.id())
            .unwrap();

        // Delegate the Ordering impl to the indices
        // NOTE: unwrap is safe since the indices are known integers.
        ai.partial_cmp(&bi).unwrap()
    });
}

/// Start the set of `PendingContainer`s with a relaxed start policy.
///
/// Returns the vector of container ids of starting containers.
fn start_relaxed_containers(
    sender: &mpsc::Sender<Result<RunningContainer, DockerError>>,
    rt: &mut current_thread::Runtime,
    containers: Vec<PendingContainer>,
) -> Vec<CleanupContainer> {
    // Convert into CleanupContainer
    let starting_containers = containers
        .iter()
        .map(|c| CleanupContainer {
            id: c.id.to_string(),
        })
        .collect();

    for c in containers.into_iter() {
        let sender_clone = sender.clone();
        let start_fut = c
            .start()
            .then(move |e| {
                // Here we send the result of the operation over the channel
                sender_clone
                    .send(e)
                    // QUESTION: How do we handle errors on this end?
                    .map_err(|e| eprintln!("failed to send container in channel: {}", e))
            })
            .and_then(|_| Ok(()));

        rt.spawn(start_fut);
    }

    starting_containers
}

fn start_strict_containers(
    rt: &mut current_thread::Runtime,
    pending: Vec<PendingContainer>,
) -> Result<Vec<RunningContainer>, Vec<CleanupContainer>> {
    let mut running = vec![];
    let mut cleanup = vec![];

    for c in pending.into_iter() {
        // Ignore future startup operations - one has already failed
        if !cleanup.is_empty() {
            cleanup.push(c.into());
            continue;
        }

        match rt.block_on(c.start()) {
            Ok(r) => running.push(r),
            Err(_e) => {
                // TODO: Retrieve container id from error message on container.start() failure.
                // Currently, this will leak the strict container that fails to start
                // and will not clean it up.
                // cleanup.push(e);
                continue;
            }
        }
    }

    if cleanup.is_empty() {
        Ok(running)
    } else {
        cleanup.append(&mut running.into_iter().map(|x| x.into()).collect());
        Err(cleanup)
    }
}

fn wait_for_relaxed_containers(
    rt: &mut current_thread::Runtime,
    receiver: mpsc::Receiver<Result<RunningContainer, DockerError>>,
    starting_relaxed: Vec<CleanupContainer>,
) -> Result<Vec<RunningContainer>, Vec<CleanupContainer>> {
    let running_relaxed_result: Vec<Result<RunningContainer, DockerError>> = rt.block_on(
        receiver
            .take(starting_relaxed.len() as u64)
            .collect()
            .map_err(|_e| starting_relaxed.clone()),
    )?;

    let mut running_relaxed: Vec<RunningContainer> = vec![];
    let mut success = true;

    for c in running_relaxed_result.into_iter() {
        match c {
            Ok(c) => running_relaxed.push(c),
            Err(_e) => success = false,
        }
    }

    if success {
        Ok(running_relaxed)
    } else {
        Err(starting_relaxed)
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
    use crate::dockertest::{
        resolve_container_handle_key, start_relaxed_containers, start_strict_containers,
        wait_for_relaxed_containers,
    };
    use crate::image::{Image, PullPolicy, Source};
    use crate::test_utils;
    use crate::waitfor::NoWait;
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
