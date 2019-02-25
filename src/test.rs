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

    pub fn namespace(self, name: String) -> DockerTest {
        DockerTest {
            namespace: name,
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
        for (_name, container) in ops.containers {
            rt.spawn(
                container
                    .remove()
                    .map_err(|e| eprintln!("failed to cleanup container instance: {}", e)),
            );
        }

        rt.run()
            .map_err(|e| DockerError::teardown(format!("failed to run teardown routines: {}", e)))
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
