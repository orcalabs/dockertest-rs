//! The main library structures.

use crate::container::{CleanupContainer, PendingContainer, RunningContainer};
use crate::image::Source;
use crate::{Composition, DockerTestError, StartPolicy};

use bollard::{
    container::{InspectContainerOptions, RemoveContainerOptions, StopContainerOptions},
    network::CreateNetworkOptions,
    volume::{CreateVolumeOptions, RemoveVolumeOptions},
    Docker,
};
use futures::future::{join_all, Future};
use rand::{self, Rng};
use std::any::Any;
use std::clone::Clone;
use std::collections::{HashMap, HashSet};
use std::panic;
use tokio::{runtime::Runtime, task::JoinHandle};
use tracing::{event, span, Level};
use tracing_futures::Instrument;

/// Represents a single docker test body execution environment.
///
/// After constructing an instance of this, we will have established a local
/// docker daemon connection with default installation properties.
///
/// Before running the test body through [run], one should configure
/// the docker container dependencies through adding [Composition] with the configured
/// environment the running container should end up representing.
///
/// By default, all images will have the local [Source], meaning that unless otherwise specified
/// in the composition stage, we expect the [Image] referenced must be available on the local
/// docker daemon.
///
/// [Source]: enum.Source.html
/// [Image]: struct.Image.html
/// [Composition]: struct.Composition.html
/// [run]: struct.DockerTest.html#method.run
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
    client: Docker,
    /// The default pull source to use for all images.
    /// Images with a specified source will override this default.
    default_source: Source,
    /// All user specified named volumes, will be created on dockertest startup.
    volumes: Vec<String>,
    /// The associated network created for this test, that all containers run within.
    network: String,
}

impl Default for DockerTest {
    fn default() -> DockerTest {
        DockerTest {
            default_source: Source::Local,
            compositions: Vec::new(),
            namespace: "dockertest-rs".to_string(),
            client: Docker::connect_with_local_defaults().expect("local docker daemon connection"),
            volumes: Vec::new(),
            network: format!("dockertest-rs-{}", generate_random_string(20)),
        }
    }
}

/// The test body parameter provided in the [DockerTest::run] argument closure.
///
/// This object allows one to interact with the containers within the test environment.
///
/// [Dockertest::run]: struct.DockerTest.html#method.run
#[derive(Clone)]
pub struct DockerOperations {
    /// Map with all started containers,
    /// the key is the container name.
    containers: Keeper<RunningContainer>,
}

/// The prune strategy for teardown of containers.
enum PruneStrategy {
    /// Always leave the container running
    RunningRegardless,
    /// Do not perform any action if the test failed.
    RunningOnFailure,
    /// With a stop-only strategy, docker volumes will NOT be pruned.
    StopOnFailure,
    /// Prune everything, including volumes.
    RemoveRegardless,
}

impl DockerOperations {
    /// Panicking implementation detail of the public `handle` method.
    fn try_handle<'a>(&'a self, handle: &'a str) -> Result<&'a RunningContainer, DockerTestError> {
        if self.containers.lookup_collisions.contains(handle) {
            return Err(DockerTestError::TestBody(format!(
                "handle '{}' defined multiple times",
                handle
            )));
        }

        match self.containers.lookup_handlers.get(handle) {
            None => Err(DockerTestError::TestBody(format!(
                "container with handle '{}' not found",
                handle
            ))),
            Some(c) => Ok(&self.containers.kept[*c]),
        }
    }

    /// Retrieve the `RunningContainer` identified by this handle.
    ///
    /// A handle for a [RunningContainer] will be either:
    /// a) the `repository` name for the [Image] when creating the `Composition`
    /// b) the container name configured on `Composition` [with_container_name].
    ///
    /// # Panics
    /// This function panics if the requested handle does not exist, or there
    /// are conflicting containers with the same repository name is present without custom
    /// configured container names.
    ///
    /// [RunningContainer]: struct.RunningContainer.html
    /// [Image]: struct.Image.html
    /// [with_container_name]: struct.Composition.html#method.with_container_name
    pub fn handle<'a>(&'a self, handle: &'a str) -> &'a RunningContainer {
        event!(Level::DEBUG, "requesting handle '{}", handle);
        match self.try_handle(handle) {
            Ok(h) => h,
            Err(e) => {
                event!(Level::ERROR, "{}", e.to_string());
                panic!(e.to_string());
            }
        }
    }

    /// Indicate that this test failed with the accompanied message.
    pub fn failure(&self, msg: &str) {
        event!(Level::ERROR, "test failure: {}", msg);
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

    /// Creates the given named volumes.
    /// Each of these volumes can be used by any container by specifying the volume
    /// name in Composition creation.
    pub fn with_named_volumes(self, names: Vec<String>) -> DockerTest {
        DockerTest {
            volumes: names,
            ..self
        }
    }

    /// Execute the test body within the provided function closure.
    /// All Compositions added to the DockerTest has successfully completed their WaitFor clause
    /// once the test body is executed.
    pub fn run<T, Fut>(self, test: T)
    where
        T: FnOnce(DockerOperations) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let span = span!(Level::ERROR, "run");
        let _guard = span.enter();

        // Allocate a new runtime for this test.
        let mut rt = match Runtime::new() {
            Ok(r) => r,
            Err(e) => {
                event!(Level::ERROR, "failed to allocate tokio runtime: {}", e);
                panic!(e.to_string());
            }
        };

        match rt.block_on(self.run_impl(test).in_current_span()) {
            Ok(_) => event!(Level::DEBUG, "dockertest successfully executed"),
            Err(e) => {
                event!(
                    Level::ERROR,
                    "internal dockertest condition failure: {:?}",
                    e
                );
                event!(Level::WARN, "dockertest failure");
                panic!(e.to_string());
            }
        }
    }

    /// Internal impl of the public `run` method, to catch internal panics
    async fn run_impl<T, Fut>(mut self, test: T) -> Result<(), DockerTestError>
    where
        T: FnOnce(DockerOperations) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // Before constructing the compositions, we ensure that all configured
        // docker volumes have been created.
        self.create_volumes().await?;

        // Resolve all name mappings prior to creation.
        // We might want to support refering to a Composition handler name
        // prior to creating the PendingContainer (in the future).
        // It therefore makes sense to split the verification/handling upfront,
        // so it is streamlined with the teardown regardless of when it must be performed.
        let mut compositions: Keeper<Composition> = self.validate_composition_handlers();

        self.resolve_final_container_name(&mut compositions);

        self.resolve_inject_container_name_env(&mut compositions)?;

        // Make sure all the images are present on the local docker daemon before we create
        // the containers from them.
        self.pull_images(&compositions).await?;

        // Create the network
        self.create_network().await?;

        // Create PendingContainers from the Compositions
        let pending_containers: Keeper<PendingContainer> =
            match self.create_containers(compositions).await {
                Ok(p) => p,
                Err(e) => {
                    self.teardown(e.1, true).await;
                    return Err(e.0);
                }
            };
        // Start the PendingContainers
        let mut running_containers: Keeper<RunningContainer> =
            match self.start_containers(pending_containers).await {
                Ok(r) => r,
                Err(e) => {
                    self.teardown(e, true).await;
                    return Err(DockerTestError::Startup(
                        "failed to start containers".to_string(),
                    ));
                }
            };

        // Create the set of cleanup containers used after the test body
        let cleanup_containers = running_containers
            .kept
            .iter()
            .map(|x| CleanupContainer {
                id: x.id().to_string(),
            })
            .collect();

        // Lets inspect each container for their ip address
        for c in running_containers.kept.iter_mut() {
            match self
                .client
                .inspect_container(&c.id, None::<InspectContainerOptions>)
                .await
            {
                Ok(details) => {
                    // Get the ip address from the network
                    c.ip = if let Some(network) =
                        details.network_settings.networks.get(&self.network)
                    {
                        event!(
                            Level::DEBUG,
                            "container ip from inspect: {}",
                            network.ip_address
                        );
                        network
                            .ip_address
                            .parse::<std::net::Ipv4Addr>()
                            // Exited containers will not have an IP address
                            .unwrap_or_else(|e| {
                                event!(Level::TRACE, "container ip address failed to parse: {}", e);
                                std::net::Ipv4Addr::UNSPECIFIED
                            })
                    } else {
                        std::net::Ipv4Addr::UNSPECIFIED
                    }
                }
                Err(e) => {
                    // This error is extraordinary - worth terminating everything.
                    self.teardown(cleanup_containers, true).await;
                    return Err(DockerTestError::Daemon(format!(
                        "failed to inspect container: {}",
                        e
                    )));
                }
            }
        }

        // We are ready to invoke the test body now
        let ops = DockerOperations {
            containers: running_containers,
        };

        // Run test body
        let result: Result<(), Option<Box<dyn Any + Send + 'static>>> =
            match tokio::spawn(test(ops)).await {
                Ok(_) => {
                    event!(Level::INFO, "test body success");
                    Ok(())
                }
                Err(e) => {
                    // Test failed
                    event!(
                        Level::ERROR,
                        "test body failed (cancelled: {}, panicked: {})",
                        e.is_cancelled(),
                        e.is_panic()
                    );
                    Err(e.try_into_panic().ok())
                }
            };

        self.teardown(cleanup_containers, result.is_err()).await;

        if let Err(option) = result {
            match option {
                Some(panic) => panic::resume_unwind(panic),
                None => panic!("test future cancelled"),
            }
        }

        Ok(())
    }

    /// Add a Composition to this DockerTest.
    pub fn add_composition(&mut self, instance: Composition) {
        self.compositions.push(instance);
    }

    #[cfg(test)]
    pub(crate) fn source(&self) -> &Source {
        &self.default_source
    }

    /// Perform the magic transformation info the final container name.
    fn resolve_final_container_name(&self, compositions: &mut Keeper<Composition>) {
        for c in compositions.kept.iter_mut() {
            let suffix = generate_random_string(20);
            c.configure_container_name(&self.namespace, &suffix);
        }
    }

    /// This function assumes that `resolve_final_container_name` has already been called.
    fn resolve_inject_container_name_env(
        &self,
        compositions: &mut Keeper<Composition>,
    ) -> Result<(), DockerTestError> {
        // Due to ownership issues, we must iterate once to verify that the handlers resolve
        // correctly, and thereafter we must apply the mutable changes to the env
        let mut composition_transforms: Vec<Vec<(String, String, String)>> = Vec::new();

        for c in compositions.kept.iter() {
            let transformed: Result<Vec<(String, String, String)>, DockerTestError>
                = c.inject_container_name_env.iter().map(|(handle, env)| {
                // Guard against duplicate handle usage.
                if compositions.lookup_collisions.contains(handle) {
                    return Err(DockerTestError::Startup(format!("composition `{}` attempted to inject_container_name_env on duplicate handle `{}`", c.handle(), handle)));
                }

                // Resolve the handle
                let index: usize = match compositions.lookup_handlers.get(handle) {
                    Some(i) => *i,
                    // TODO: usererror
                    None => return Err(DockerTestError::Startup(format!("composition `{}` attempted to inject_container_name_env on non-existent handle `{}`", c.handle(), handle))),
                };

                let container_name = compositions.kept[index].container_name.clone();

                Ok((handle.clone(), container_name, env.clone()))
            }).collect();

            composition_transforms.push(transformed?);
        }

        for (index, c) in compositions.kept.iter_mut().enumerate() {
            for (handle, name, env) in composition_transforms[index].iter() {
                // Inject the container name into env
                if let Some(old) = c.env.insert(env.to_string(), name.to_string()) {
                    event!(Level::WARN, "overwriting previously configured environment variable `{} = {}` with injected container name for handle `{}`", env, old, handle);
                }
            }
        }

        Ok(())
    }

    async fn create_network(&self) -> Result<(), DockerTestError> {
        let config = CreateNetworkOptions {
            name: self.network.as_str(),
            ..Default::default()
        };

        event!(Level::TRACE, "creating network {}", self.network);
        let res = self
            .client
            .create_network(config)
            .await
            .map(|_| ())
            .map_err(|e| {
                DockerTestError::Startup(format!("creating docker network failed: {}", e))
            });

        event!(
            Level::TRACE,
            "finished created network with result: {}",
            res.is_ok()
        );

        res
    }

    /// Creates the set of `PendingContainer`s from the `Composition`s.
    ///
    /// This function assumes that all images required by the `Composition`s are
    /// present on the local docker daemon.
    async fn create_containers(
        &self,
        compositions: Keeper<Composition>,
    ) -> Result<Keeper<PendingContainer>, (DockerTestError, Vec<CleanupContainer>)> {
        event!(Level::TRACE, "creating containers");

        // NOTE: The insertion order is preserved.
        let mut pending: Vec<PendingContainer> = Vec::new();

        for instance in compositions.kept.into_iter() {
            match instance.create(&self.client, &self.network).await {
                Ok(c) => pending.push(c),
                Err(e) => {
                    // Error condition arose - we return the successfully created containers
                    // (for cleanup purposes)
                    return Err((e, pending
                        .into_iter()
                        .map(|x| x.into())
                        .collect::<Vec<CleanupContainer>>()));
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
    async fn start_containers(
        &mut self,
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
        let original_ordered_ids = pending_containers
            .kept
            .iter()
            .map(|c| c.id.to_string())
            .collect();

        // Replace the `kept` vector into the stack frame
        let pending = std::mem::replace(&mut pending_containers.kept, vec![]);
        let (relaxed, strict): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|c| c.start_policy == StartPolicy::Relaxed);

        let mut cleanup: Vec<CleanupContainer> = vec![];
        let mut running_containers = vec![];

        // We need to gather all the containers for cleanup purposes.
        // Simply make a bloody copy of it now and be done with it
        cleanup.extend(relaxed.iter().map(CleanupContainer::from));
        cleanup.extend(strict.iter().map(CleanupContainer::from));

        // Asynchronously start all relaxed containers.
        // Each completed container will signal back on the mpsc channel.
        let starting_relaxed = start_relaxed_containers(relaxed);

        let success = match start_strict_containers(strict).await {
            Ok(mut r) => {
                running_containers.append(&mut r);
                true
            }
            Err(_) => false,
        };
        let success = match wait_for_relaxed_containers(starting_relaxed, !success).await {
            Ok(mut r) => {
                running_containers.append(&mut r);
                true
            }
            Err(_) => false,
        };

        if success {
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
    async fn pull_images(&self, compositions: &Keeper<Composition>) -> Result<(), DockerTestError> {
        let mut future_vec = Vec::new();

        for composition in compositions.kept.iter() {
            let fut = composition.image().pull(&self.client, &self.default_source);

            future_vec.push(fut);
        }

        join_all(future_vec).await;
        Ok(())
    }

    /// Forcefully remove the `CleanupContainer` objects from `cleanup`.
    /// Also removes all named volumes added to dockertest.
    /// All errors are discarded.
    async fn teardown(&self, cleanup: Vec<CleanupContainer>, test_failed: bool) {
        // Get the prune strategy for this test.
        let prune = match std::env::var_os("DOCKERTEST_PRUNE") {
            Some(val) => match val.to_string_lossy().to_lowercase().as_str() {
                "stop_on_failure" => PruneStrategy::StopOnFailure,
                "never" => PruneStrategy::RunningRegardless,
                "running_on_failure" => PruneStrategy::RunningOnFailure,
                "always" => PruneStrategy::RemoveRegardless,
                _ => {
                    event!(Level::WARN, "unrecognized `DOCKERTEST_PRUNE = {:?}`", val);
                    event!(Level::DEBUG, "defaulting to prune stategy RemoveRegardless");
                    PruneStrategy::RemoveRegardless
                }
            },
            // Default strategy
            None => PruneStrategy::RemoveRegardless,
        };

        match prune {
            PruneStrategy::RunningRegardless => {
                event!(
                    Level::DEBUG,
                    "Leave all containers running regardless of outcome"
                );
                return;
            }

            PruneStrategy::RunningOnFailure if test_failed => {
                event!(
                    Level::DEBUG,
                    "Leaving all containers running due to test failure"
                );
                return;
            }

            // We only stop, and do not remove, if test failed and our strategy
            // tells us to do so.
            PruneStrategy::StopOnFailure if test_failed => {
                join_all(
                    cleanup
                        .iter()
                        .map(|c| {
                            self.client
                                .stop_container(&c.id, None::<StopContainerOptions>)
                        })
                        .collect::<Vec<_>>(),
                )
                .await;

                self.teardown_network().await;
                return;
            }

            // Catch all to remove everything.
            PruneStrategy::StopOnFailure
            | PruneStrategy::RunningOnFailure
            | PruneStrategy::RemoveRegardless => {
                event!(Level::DEBUG, "forcefully removing all containers");
            }
        }

        // We spawn all cleanup procedures independently, because we want to cleanup
        // as much as possible, even if one fail.
        let mut remove_futs = Vec::new();
        for c in cleanup.iter() {
            let options = Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            });
            remove_futs.push(self.client.remove_container(&c.id, options));
        }
        // Volumes have to be removed after the containers, as we will get a 409 from the docker
        // daemon if the volume is still in use by a container.
        // We therefore run the container remove futures to completion before trying to remove volumes.
        // We will not be able to remove volumes if the associated container was not removed
        // successfully.
        join_all(remove_futs).await;

        // Network must be removed after containers have been stopped.
        self.teardown_network().await;

        // Cleanup volumes now
        let mut volume_futs = Vec::new();
        for v in &self.volumes {
            let options = Some(RemoveVolumeOptions { force: true });
            volume_futs.push(self.client.remove_volume(v, options))
        }
        join_all(volume_futs).await;
    }

    /// Make sure we remove the network we have previously created.
    async fn teardown_network(&self) {
        if let Err(e) = self.client.remove_network(&self.network).await {
            event!(
                Level::ERROR,
                "unable to remove docker network `{}`: {}",
                self.network,
                e
            );
        }
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

    async fn create_volumes(&self) -> Result<(), DockerTestError> {
        let mut volume_futs = Vec::new();
        for v in &self.volumes {
            let config = CreateVolumeOptions {
                name: v.as_str(),
                ..Default::default()
            };
            volume_futs.push(self.client.create_volume(config));
        }

        join_all(volume_futs).await;

        Ok(())
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
    containers: Vec<PendingContainer>,
) -> Vec<JoinHandle<Result<RunningContainer, DockerTestError>>> {
    event!(Level::TRACE, "beginning starting relaxed containers");
    containers
        .into_iter()
        .map(|c| tokio::spawn(c.start()))
        .collect()
}

async fn start_strict_containers(
    pending: Vec<PendingContainer>,
) -> Result<Vec<RunningContainer>, ()> {
    let mut running = vec![];
    let mut success = true;

    event!(Level::TRACE, "beginning starting strict containers");
    for c in pending.into_iter() {
        match c.start().await {
            Ok(r) => running.push(r),
            Err(_e) => {
                // TODO: Retrieve container id from error message on container.start() failure.
                // Currently, this will leak the strict container that fails to start
                // and will not clean it up.
                // cleanup.push(e);
                event!(Level::ERROR, "starting strict container failed {}", _e);
                success = false;
                break;
            }
        }
    }

    event!(
        Level::TRACE,
        "finished starting strict containers with result: {}",
        success
    );

    if success {
        Ok(running)
    } else {
        Err(())
    }
}

/// Await the completionj of all spawned futures.
///
/// If success is false, simply cancel the futures and return an error
async fn wait_for_relaxed_containers(
    starting_relaxed: Vec<JoinHandle<Result<RunningContainer, DockerTestError>>>,
    cancel_futures: bool,
) -> Result<Vec<RunningContainer>, ()> {
    if cancel_futures {
        event!(
            Level::ERROR,
            "cancel futures requested - dropping relaxed join handles"
        );
        drop(starting_relaxed);
        return Err(());
    }

    let mut running_relaxed: Vec<RunningContainer> = Vec::new();
    let mut success = true;

    for join_handle in join_all(starting_relaxed).await {
        match join_handle {
            Ok(start_result) => match start_result {
                Ok(c) => running_relaxed.push(c),
                Err(e) => {
                    event!(
                        Level::ERROR,
                        "starting relaxed container result error: {}",
                        e
                    );
                    success = false
                }
            },
            Err(_) => {
                event!(Level::ERROR, "join errror on gathering relaxed containers");
                success = false;
            }
        }
    }

    event!(
        Level::TRACE,
        "finished waiting for started relaxed containers with result: {}",
        success
    );

    if success {
        Ok(running_relaxed)
    } else {
        Err(())
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
    use super::Keeper;
    use crate::container::{CleanupContainer, PendingContainer, RunningContainer};
    use crate::dockertest::{
        start_relaxed_containers, start_strict_containers, wait_for_relaxed_containers,
    };
    use crate::image::{Image, PullPolicy, Source};
    use crate::test_utils;
    use crate::{Composition, DockerTest, StartPolicy};

    use futures::channel::mpsc;
    use futures::future::Future;
    use futures::sink::Sink;
    use futures::stream::Stream;
    use std::rc::Rc;
    use tokio::runtime::current_thread;

    // Tests that the default DockerTest constructor produces a valid instance with the correct values set
    #[test]
    fn test_default_constructor() {
        let test = DockerTest::new();
        assert_eq!(
            test.compositions.len(),
            0,
            "should not have any strict instances after creation"
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
    fn test_pull_images() {
        // meta
        let repository = "hello-world".to_string();
        let tag = "latest".to_string();
        let image = Image::with_repository(&repository);
        let composition = Composition::with_image(image).with_start_policy(StartPolicy::Relaxed);

        // setup
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());
        let mut test =
            DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::IfNotPresent));
        test.add_composition(composition);
        let compositions: Keeper<Composition> = test.validate_composition_handlers();

        // Ensure image is not present
        let res = rt.block_on(test_utils::delete_image_if_present(
            &repository,
            &tag,
            &client,
        ));
        assert!(
            res.is_ok(),
            format!("failed to delete existing image {}", res.unwrap_err())
        );

        // Issue test operation
        let output = test.pull_images(&mut rt, &compositions);
        assert!(output.is_ok(), format!("{}", output.unwrap_err()));

        // Verify
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
        // meta
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

        // setup
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());
        let mut test =
            DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::IfNotPresent));
        rt.block_on(image.pull(client.clone(), test.source()))
            .expect("failed to pull relaxed container image");

        // Add Composition to test
        let image_id = image.retrieved_id();
        let composition = Composition::with_image(image).with_start_policy(StartPolicy::Relaxed);
        test.add_composition(composition);

        // Ensure that that previous instances of the container is removed
        rt.block_on(test_utils::remove_containers(image_id, &client))
            .expect("failed to remove existing containers");

        // Create the container and retrieve its new id
        let compositions: Keeper<Composition> = test.validate_composition_handlers();
        let mut containers: Keeper<PendingContainer> = test
            .create_containers(&mut rt, compositions)
            .expect("failed to create containers");
        let container_id = containers
            .kept
            .first()
            .expect("failed to get container from vector")
            .id
            .to_string();
        let pending = std::mem::replace(&mut containers.kept, vec![]);

        // Issue the test operation
        let (sender, receiver) = mpsc::channel(0);
        start_relaxed_containers(&sender, &mut rt, pending);

        // Wait for the operation to complete
        let mut result = rt
            .block_on(
                receiver
                    .take(1)
                    .collect()
                    .map_err(|e| format!("failed to retrieve started relaxed container: {:?}", e)),
            )
            .expect("failed to retrieve result of startup procedure");
        let result = result
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
    // TODO: Add failure test for starting strict containers
    #[test]
    fn test_start_strict_containers() {
        // meta
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

        // setup
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());
        let mut test =
            DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::IfNotPresent));
        rt.block_on(image.pull(client.clone(), test.source()))
            .expect("failed to pull relaxed container image");

        // Add Composition to test
        let image_id = image.retrieved_id();
        let composition = Composition::with_image(image).with_start_policy(StartPolicy::Strict);
        test.add_composition(composition);

        // Ensure that that previous instances of the container is removed
        rt.block_on(test_utils::remove_containers(image_id, &client))
            .expect("failed to remove existing containers");

        // Create the container and retrieve its new id
        let compositions: Keeper<Composition> = test.validate_composition_handlers();
        let mut containers: Keeper<PendingContainer> = test
            .create_containers(&mut rt, compositions)
            .expect("failed to create containers");
        let container_id = containers
            .kept
            .first()
            .expect("failed to get container from vector")
            .id
            .to_string();
        let pending = std::mem::replace(&mut containers.kept, vec![]);

        // Issue the operation
        let res = start_strict_containers(&mut rt, pending);

        assert!(res.is_ok(), format!("failed to start relaxed container"));

        let is_running = rt
            .block_on(test_utils::is_container_running(container_id, &client))
            .expect("failed to check liveness of relaxed container");

        assert!(
            is_running,
            "relaxed container should be running after starting it"
        );
    }

    // Tests that `wait_for_relaxed_containers` function successfully waits the set.
    // TODO: Add failure test for starting relaxed containers
    #[test]
    fn test_wait_for_relaxed_container() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let (sender, receiver) = mpsc::channel(0);
        let sender_ref = &sender;

        // Spawn X running containers and receive the result
        for i in 1..10 {
            let running = RunningContainer {
                id: i.to_string(),
                name: i.to_string(),
                ip: std::net::Ipv4Addr::UNSPECIFIED,
            };
            let sender = sender_ref.clone();
            let send_fut = sender
                .send(Ok(running))
                .map_err(|e| eprintln!("failed to send empty OK through channel {}", e))
                .map(|_| ());

            rt.spawn(send_fut);
        }

        let cleanup = (1..10)
            .into_iter()
            .map(|x| CleanupContainer { id: x.to_string() })
            .collect();
        let res = wait_for_relaxed_containers(&mut rt, receiver, cleanup);
        assert!(res.is_ok(), "failed to receive results through channel");
    }

    // Tests that the teardown method removes all containers
    #[test]
    fn test_teardown_with_exited_containers() {
        // meta
        let repository = "hello-world".to_string();
        let image = Image::with_repository(&repository);

        // setup
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");
        let client = Rc::new(shiplift::Docker::new());
        let mut test =
            DockerTest::new().with_default_source(Source::DockerHub(PullPolicy::IfNotPresent));
        rt.block_on(image.pull(client.clone(), test.source()))
            .expect("failed to pull relaxed container image");

        // Add Composition to test
        let composition = Composition::with_image(image).with_start_policy(StartPolicy::Relaxed);
        test.add_composition(composition);

        // Create
        // XXX: This does NOT ensure that the created container does not already exist, no?
        let compositions: Keeper<Composition> = test.validate_composition_handlers();
        let pending: Keeper<PendingContainer> = test
            .create_containers(&mut rt, compositions)
            .expect("failed to create containers");

        // Start
        let running = test
            .start_containers(&mut rt, pending)
            .expect("failed to start containers");

        // Issue the test operation
        let cleanup: Vec<CleanupContainer> = running.kept.into_iter().map(|x| x.into()).collect();
        let cleanup_copy = cleanup.clone();
        test.teardown(&mut rt, cleanup);
        // NOTE: Currently, teardown does not return a result

        // Check that containers are not running
        for c in cleanup_copy {
            let is_running = rt
                .block_on(test_utils::is_container_running(c.id.to_string(), &client))
                .expect("failed to check for container liveness");
            assert!(
                !is_running,
                "container should not be running after teardown"
            );
        }
    }

    /*
     * TODO: Implement tests for handle resolution on on the Keeper<T> object, especially after
     * the convertion into RunningContainer.
     */
}
