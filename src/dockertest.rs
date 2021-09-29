//! The main library structures.

use crate::composition::{LogPolicy, LogSource};
use crate::container::{CleanupContainer, HostPortMappings, PendingContainer, RunningContainer};
use crate::image::Source;
use crate::{Composition, DockerTestError, StartPolicy};

use crate::{static_container::STATIC_CONTAINERS, utils::connect_with_local_or_tls_defaults};
use bollard::{
    container::{InspectContainerOptions, RemoveContainerOptions, StopContainerOptions},
    network::{CreateNetworkOptions, DisconnectNetworkOptions},
    volume::RemoveVolumeOptions,
    Docker,
};
use futures::future::{join_all, Future};
use rand::{self, Rng};
use std::any::Any;
use std::clone::Clone;
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::panic;
use tokio::{runtime::Runtime, task::JoinHandle};
use tracing::{event, span, Level};
use tracing_futures::Instrument;

/// Represents a single docker test body execution environment.
///
/// After constructing an instance of this, we will have established a
/// connection to a Docker daemon.
///
/// When `tls` feature is enabled and `DOCKER_TLS_VERIFY` environment variable is set to a nonempty
/// value the connection will use TLS encryption. [DOCKER_* env
/// variables](https://docs.rs/bollard/0.11.0/bollard/index.html#ssl-via-rustls) configure a TCP
/// connection URI and a location of client private key and client/CA certificates.
///
/// Otherwise local connection is used - via unix socket or named pipe (on Windows).
///
/// Before running the test body through [run](DockerTest::run), one should configure
/// the docker container dependencies through adding [Composition] with the configured
/// environment the running container should end up representing.
///
/// By default, all images will have the local [Source], meaning that unless otherwise specified
/// in the composition stage, we expect the [Image](crate::image::Image) referenced must be
/// available on the local docker daemon.
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
    /// Each volume named is suffixed with the dockertest ID.
    /// This vector ONLY contains named_volumes and only their names, the container_path is stored
    /// in the Composition.
    named_volumes: Vec<String>,
    /// The associated network created for this test, that all containers run within.
    network: String,
    /// If set, the `network` referenced points to an externally managed network.
    /// We do not meddle with this network, only include ourselves in it.
    is_external_network: bool,
    /// Retrieved internally by an env variable the user has to set.
    /// Will only be used in environments where dockertest itself is running inside a container.
    container_id: Option<String>,
    /// ID of this DockerTest instance.
    /// When tests are run in parallel multiple DockerTest instances will exist at the same time,
    /// to distinguish which resources belongs to each test environment the resource name should be
    /// suffixed with this ID.
    /// This applies to resouces such as docker network names and named volumes.
    id: String,
}

impl Default for DockerTest {
    fn default() -> DockerTest {
        Self::try_new().unwrap()
    }
}

/// The test body parameter provided in the [DockerTest::run] argument closure.
///
/// This object allows one to interact with the containers within the test environment.
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
    /// Prune everything, including named and anonymous volumes.
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
    /// a) the `repository` name for the [Image](crate::image::Image) when creating the `Composition`
    /// b) the container name configured on `Composition` [with_container_name].
    ///
    /// # Panics
    /// This function panics if the requested handle does not exist, or there
    /// are conflicting containers with the same repository name is present without custom
    /// configured container names.
    ///
    /// [with_container_name]: Composition::with_container_name
    pub fn handle<'a>(&'a self, handle: &'a str) -> &'a RunningContainer {
        event!(Level::DEBUG, "requesting handle '{}", handle);
        match self.try_handle(handle) {
            Ok(h) => h,
            Err(e) => {
                event!(Level::ERROR, "{}", e.to_string());
                panic!("{}", e);
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
    /// Creates a new DockerTest. Panics on Docker daemon connection failure.
    pub fn new() -> DockerTest {
        DockerTest {
            ..Default::default()
        }
    }

    /// Creates a new DockerTest. Returns error on Docker daemon connection failure.
    pub fn try_new() -> Result<DockerTest, DockerTestError> {
        let id = generate_random_string(20);
        let client = connect_with_local_or_tls_defaults()?;
        Ok(DockerTest {
            default_source: Source::Local,
            compositions: Vec::new(),
            namespace: "dockertest-rs".to_string(),
            client,
            container_id: None,
            named_volumes: Vec::new(),
            network: format!("dockertest-rs-{}", id),
            is_external_network: false,
            id,
        })
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

    /// Test will use an externally managed docker network.
    ///
    /// All created containers will attach itself to the existing, externally managed network.
    ///
    /// If the container is created with a [crate::composition::StaticManagementPolicy::External],
    /// it is assumed that the container is already part of this network.
    ///
    /// For [crate::composition::StaticManagementPolicy::DockerTest], the container will be included
    /// into the network before test starts, and dropped once the statically managed container
    /// is removed.
    pub fn with_external_network<T: ToString>(self, network: T) -> DockerTest {
        DockerTest {
            network: network.to_string(),
            is_external_network: true,
            ..self
        }
    }

    /// Set that we want to store logs from containers into individual files
    /// Execute the test body within the provided function closure.
    /// All Compositions added to the DockerTest has successfully completed their WaitFor clause
    /// once the test body is executed.
    // NOTE(clippy): tracing generates cognitive complexity due to macro expansion.
    #[allow(clippy::cognitive_complexity)]
    pub fn run<T, Fut>(self, test: T)
    where
        T: FnOnce(DockerOperations) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let span = span!(Level::ERROR, "run");
        let _guard = span.enter();

        // Allocate a new runtime for this test.
        let rt = match Runtime::new() {
            Ok(r) => r,
            Err(e) => {
                event!(Level::ERROR, "failed to allocate tokio runtime: {}", e);
                panic!("{}", e);
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
                panic!("{}", e);
            }
        }
    }

    /// Async version of of `run`, if your test already uses a runtime e.g. you tagged your test with
    /// tokio::test, then use this version.
    // Having a separate method for the async version is a workaround to enable users to run async
    // tests.
    // The sync run version will panic if its already running on a runtime.
    // Getting a handle to a runtime if it exists and then running the 'run_impl' on that runtime
    // currently does not work.
    pub async fn run_async<T, Fut>(self, test: T)
    where
        T: FnOnce(DockerOperations) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let span = span!(Level::ERROR, "run");
        let _guard = span.enter();

        match self.run_impl(test).in_current_span().await {
            Ok(_) => event!(Level::DEBUG, "dockertest successfully executed"),
            Err(e) => {
                event!(
                    Level::ERROR,
                    "internal dockertest condition failure: {:?}",
                    e
                );
                event!(Level::WARN, "dockertest failure");
                panic!("{}", e);
            }
        }
    }

    /// Internal impl of the public `run` method, to catch internal panics
    async fn run_impl<T, Fut>(mut self, test: T) -> Result<(), DockerTestError>
    where
        T: FnOnce(DockerOperations) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // If we are inside a container, we need to retrieve our container ID.
        self.check_if_inside_container();

        // Before constructing the compositions, we ensure that all configured
        // docker volumes have been created.
        self.resolve_named_volumes().await?;

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
        if !self.is_external_network {
            self.create_network().await?;
        }

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
                Err((e, containers)) => {
                    self.teardown(containers, true).await;
                    return Err(e);
                }
            };

        // External containers return None on container creation and will therefore not be present
        // in the Keeper so we need to add them.
        running_containers
            .kept
            .append(&mut STATIC_CONTAINERS.external_containers().await);

        // Create the set of cleanup containers used after the test body
        let cleanup_containers = running_containers
            .kept
            .iter()
            .map(CleanupContainer::from)
            .collect();

        // Lets inspect each container for their ip address
        for c in running_containers.kept.iter_mut() {
            // On Windows container IPs cannot be resolved from outside a container.
            // So container IPs in the test body are useless and the only way to contact a
            // container is through a port map and localhost.
            // To avoid have users to have cfg!(windows) in their test bodies, we simply set all
            // container ips to localhost
            //
            // TODO: Find another strategy to contact containers from the test body on Windows.
            if cfg!(windows) {
                c.ip = std::net::Ipv4Addr::new(127, 0, 0, 1);
                continue;
            }
            match self
                .client
                .inspect_container(&c.id, None::<InspectContainerOptions>)
                .await
            {
                Ok(details) => {
                    // Get the ip address from the network
                    c.ip = if let Some(network) = details
                        .network_settings
                        .as_ref()
                        .unwrap()
                        .networks
                        .as_ref()
                        .unwrap()
                        .get(&self.network)
                    {
                        event!(
                            Level::DEBUG,
                            "container ip from inspect: {}",
                            network.ip_address.as_ref().unwrap()
                        );
                        network
                            .ip_address
                            .as_ref()
                            .unwrap()
                            .parse::<std::net::Ipv4Addr>()
                            // Exited containers will not have an IP address
                            .unwrap_or_else(|e| {
                                event!(Level::TRACE, "container ip address failed to parse: {}", e);
                                std::net::Ipv4Addr::UNSPECIFIED
                            })
                    } else {
                        std::net::Ipv4Addr::UNSPECIFIED
                    };
                    c.ports = if let Some(ports) = details.network_settings.unwrap().ports {
                        event!(
                            Level::DEBUG,
                            "container ports from inspect: {:?}",
                            ports.clone()
                        );
                        match HostPortMappings::try_from(ports) {
                            Ok(h) => h,
                            Err(e) => {
                                self.teardown(cleanup_containers, true).await;
                                return Err(DockerTestError::HostPort(e.to_string()));
                            }
                        }
                    } else {
                        HostPortMappings::default()
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
                    event!(Level::DEBUG, "test body success");
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

        self.handle_logs(&cleanup_containers, result.is_err())
            .await?;
        self.teardown(cleanup_containers, result.is_err()).await;

        if let Err(option) = result {
            match option {
                Some(panic) => panic::resume_unwind(panic),
                None => panic!("test future cancelled"),
            }
        }

        Ok(())
    }

    /// Handle container logs.
    ///
    /// This function handles logs on per-container bases.
    async fn handle_logs(
        &self,
        containers: &[CleanupContainer],
        test_failed: bool,
    ) -> Result<(), DockerTestError> {
        for container in containers {
            // we need to handle logs only if log_options is not None
            if let Some(log_options) = &container.log_options {
                // check if we need to capture stderr and/or stdout
                let should_log_stderr = match log_options.source {
                    LogSource::StdErr => true,
                    LogSource::StdOut => false,
                    LogSource::Both => true,
                };

                let should_log_stdout = match log_options.source {
                    LogSource::StdErr => false,
                    LogSource::StdOut => true,
                    LogSource::Both => true,
                };

                let result = match log_options.policy {
                    LogPolicy::Always => {
                        container
                            .handle_log(&log_options.action, should_log_stderr, should_log_stdout)
                            .await
                    }
                    LogPolicy::OnError => {
                        if !test_failed {
                            continue;
                        }
                        container
                            .handle_log(&log_options.action, should_log_stderr, should_log_stdout)
                            .await
                    }
                };

                result.map_err(|error| {
                    DockerTestError::LogWriteError(format!(
                        "unable to handle logs for: {}: {}",
                        container.name, error
                    ))
                })?;
            }
        }
        Ok(())
    }

    /// Add a Composition to this DockerTest.
    pub fn add_composition(&mut self, instance: Composition) {
        self.compositions.push(instance);
    }

    /// Retrieve the default source for Images unless explicitly specified per Image.
    pub fn source(&self) -> &Source {
        &self.default_source
    }

    /// Perform the magic transformation info the final container name.
    fn resolve_final_container_name(&self, compositions: &mut Keeper<Composition>) {
        for c in compositions.kept.iter_mut() {
            let suffix = generate_random_string(20);
            c.configure_container_name(&self.namespace, &suffix);
        }
    }

    /// Checks if we are inside a container, and if so sets our container ID.
    /// The user of dockertest is responsible for setting these env variables.
    fn check_if_inside_container(&mut self) {
        if let Ok(id) = std::env::var("DOCKERTEST_CONTAINER_ID_INJECT_TO_NETWORK") {
            event!(
                Level::TRACE,
                "dockertest container id env is set, we are running inside a container, id: {}",
                id
            );
            self.container_id = Some(id);
        } else {
            event!(
                Level::TRACE,
                "dockertest container id env is not set, running native on host"
            );
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

        if let Some(id) = self.container_id.clone() {
            self.add_self_to_network(id).await?;
        }

        res
    }

    async fn add_self_to_network(&self, id: String) -> Result<(), DockerTestError> {
        event!(
            Level::TRACE,
            "adding dockertest container to created network, container_id: {}, network_id: {}",
            &id,
            &self.network
        );
        let opts = bollard::network::ConnectNetworkOptions {
            container: id,
            endpoint_config: bollard::models::EndpointSettings::default(),
        };

        self.client
            .connect_network(&self.network, opts)
            .await
            .map_err(|e| {
                DockerTestError::Startup(format!(
                    "failed to add internal container to dockertest network: {}",
                    e
                ))
            })
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
            match instance
                .create(&self.client, Some(&self.network), self.is_external_network)
                .await
            {
                Ok(c) => {
                    if let Some(container) = c {
                        pending.push(container)
                    }
                }
                Err(e) => {
                    // Error condition arose - we return the successfully created containers
                    // (for cleanup purposes)
                    return Err((
                        e,
                        pending
                            .into_iter()
                            .map(|x| x.into())
                            .collect::<Vec<CleanupContainer>>(),
                    ));
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
    ) -> Result<Keeper<RunningContainer>, (DockerTestError, Vec<CleanupContainer>)> {
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
        let pending = std::mem::take(&mut pending_containers.kept);
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

        let strict_success = match start_strict_containers(strict).await {
            Ok(mut r) => {
                running_containers.append(&mut r);
                Ok(())
            }
            Err(e) => Err(e),
        };
        let relaxed_success =
            match wait_for_relaxed_containers(starting_relaxed, strict_success.is_err()).await {
                Ok(mut r) => {
                    running_containers.append(&mut r);
                    Ok(())
                }
                Err(e) => Err(e),
            };

        // Calculate the first error from strict then relaxed, and return that if present.
        match strict_success.err().or_else(|| relaxed_success.err()) {
            None => {
                sort_running_containers_into_insertion_order(
                    &mut running_containers,
                    original_ordered_ids,
                );
                Ok(Keeper::<RunningContainer> {
                    kept: running_containers,
                    lookup_collisions: pending_containers.lookup_collisions,
                    lookup_handlers: pending_containers.lookup_handlers,
                })
            }
            Some(e) => Err((e, cleanup)),
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
    async fn teardown(&self, mut cleanup: Vec<CleanupContainer>, test_failed: bool) {
        let static_cleanup = cleanup
            .iter()
            .filter_map(|c| {
                if c.is_static() {
                    Some(c.id.as_str())
                } else {
                    None
                }
            })
            .collect();
        // Static containers ignores the prune strategy as other tests might still be running
        STATIC_CONTAINERS
            .cleanup(
                &self.client,
                &self.network,
                self.is_external_network,
                static_cleanup,
            )
            .await;

        // Cleanup of static containers has to be synchronized between tests
        cleanup.retain(|c| !c.is_static());

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

                if !self.is_external_network {
                    self.teardown_network().await;
                }
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
            // It's unlikely that anonymous volumes will be used by several containers. In this
            // case there will be remove errors that it's possible just to ignore (see
            // https://github.com/moby/moby/blob/7b9275c0da707b030e62c96b679a976f31f929d3/daemon/mounts.go#L34).
            let options = Some(RemoveContainerOptions {
                force: true,
                v: true,
                ..Default::default()
            });
            remove_futs.push(self.client.remove_container(&c.id, options));
        }
        // Volumes have to be removed after the containers, as we will get a 409 from the docker daemon if the volume is still in use by a container.
        // We therefore run the container remove futures to completion before trying to remove volumes.
        // We will not be able to remove volumes if the associated container was not removed
        // successfully.
        join_all(remove_futs).await;

        // Network must be removed after containers have been stopped.
        if !self.is_external_network {
            self.teardown_network().await;
        }

        // Cleanup volumes now
        let mut volume_futs = Vec::new();

        for v in &self.named_volumes {
            event!(Level::INFO, "removing named volume: {:?}", &v);
            let options = Some(RemoveVolumeOptions { force: true });
            volume_futs.push(self.client.remove_volume(v, options))
        }

        join_all(volume_futs).await;
    }

    /// Make sure we remove the network we have previously created.
    async fn teardown_network(&self) {
        if let Some(id) = self.container_id.clone() {
            let opts = DisconnectNetworkOptions::<&str> {
                container: &id,
                force: true,
            };
            if let Err(e) = self.client.disconnect_network(&self.network, opts).await {
                event!(
                    Level::ERROR,
                    "unable to remove dockertest-container from network: {}",
                    e
                );
            }
        }

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
        let compositions = std::mem::take(&mut self.compositions);
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

    // Determines the final name for all named volumes, and modifies the Compositions accordingly.
    // Named volumes will have the following form: "USER_PROVIDED_VOLUME_NAME-DOCKERTEST_ID:PATH_IN_CONTAINER".
    async fn resolve_named_volumes(&mut self) -> Result<(), DockerTestError> {
        // Maps the original volume name to the suffixed ones
        // Key: "USER_PROVIDED_VOLUME_NAME"
        // Value: "USER_PROVIDED_VOLUME_NAME-DOCKERTEST_ID"
        let mut volume_name_map: HashMap<String, String> = HashMap::new();

        let suffix = self.id.clone();

        // Add the dockertest ID as a suffix to all named volume names.
        self.compositions.iter_mut().for_each(|mut c| {
            // Includes path aswell: "USER_PROVIDED_VOLUME_NAME-DOCKERTEST_ID:PATH_IN_CONTAINER"
            let mut volume_names_with_path: Vec<String> = Vec::new();

            c.named_volumes.iter().for_each(|(id, path)| {
                if let Some(suffixed_name) = volume_name_map.get(id) {
                    volume_names_with_path.push(format!("{}:{}", &suffixed_name, &path));
                } else {
                    let volume_name_with_path = format!("{}-{}:{}", id, &suffix, path);
                    volume_names_with_path.push(volume_name_with_path);

                    let suffixed_volume_name = format!("{}-{}", id, &suffix);
                    volume_name_map.insert(id.to_string(), suffixed_volume_name);
                }
            });

            c.final_named_volume_names = volume_names_with_path;
        });

        // Add all the suffixed volumes names to dockertest such that we can clean them up later.
        self.named_volumes = volume_name_map.drain().map(|(_k, v)| v).collect();

        event!(
            Level::DEBUG,
            "added named volumes to cleanup list: {:?}",
            &self.named_volumes
        );

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
) -> Result<Vec<RunningContainer>, DockerTestError> {
    let mut running = vec![];
    let mut first_error = None;

    event!(Level::TRACE, "beginning starting strict containers");
    for c in pending.into_iter() {
        match c.start().await {
            Ok(r) => running.push(r),
            Err(e) => {
                event!(Level::ERROR, "starting strict container failed {}", e);
                first_error = Some(e);
                break;
            }
        }
    }

    event!(
        Level::TRACE,
        "finished starting strict containers with result: {}",
        first_error.is_none()
    );

    match first_error {
        None => Ok(running),
        Some(e) => Err(e),
    }
}

/// Await the completionj of all spawned futures.
///
/// If success is false, simply cancel the futures and return an error
async fn wait_for_relaxed_containers(
    starting_relaxed: Vec<JoinHandle<Result<RunningContainer, DockerTestError>>>,
    cancel_futures: bool,
) -> Result<Vec<RunningContainer>, DockerTestError> {
    if cancel_futures {
        event!(
            Level::ERROR,
            "cancel futures requested - dropping relaxed join handles"
        );
        drop(starting_relaxed);
        return Err(DockerTestError::Processing(
            "cancelling all relaxed container join operations".to_string(),
        ));
    }

    let mut running_relaxed: Vec<RunningContainer> = Vec::new();
    let mut first_error = None;

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
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            },
            Err(_) => {
                event!(Level::ERROR, "join errror on gathering relaxed containers");
                if first_error.is_none() {
                    first_error = Some(DockerTestError::Processing(
                        "join error gathering".to_string(),
                    ));
                }
            }
        }
    }

    event!(
        Level::TRACE,
        "finished waiting for started relaxed containers with result: {}",
        first_error.is_none()
    );

    match first_error {
        None => Ok(running_relaxed),
        Some(e) => Err(e),
    }
}

fn generate_random_string(len: i32) -> String {
    let mut random_string = String::new();
    let mut rng = rand::thread_rng();
    for _i in 0..len {
        let letter: char = rng.gen_range(b'a', b'z') as char;
        random_string.push(letter);
    }

    random_string
}

#[cfg(test)]
mod tests {
    use crate::{DockerTest, PullPolicy, Source};

    //use bollard::Docker;

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
}
