//! The main library structures.

use crate::container::RunningContainer;
use crate::docker::Docker;
use crate::dockertest::Network;
use crate::engine::{bootstrap, Debris, Engine, Orbiting};
use crate::static_container::SCOPED_NETWORKS;
use crate::utils::generate_random_string;
use crate::{DockerTest, DockerTestError};
use futures::future::Future;
use std::any::Any;
use std::clone::Clone;
use std::collections::HashMap;
use std::panic;
use tracing::{error, event, trace, Level};

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
pub(crate) struct Runner {
    /// The docker client to interact with the docker daemon with.
    client: Docker,
    /// The config to run this test with.
    config: DockerTest,

    /// All user specified named volumes, will be created on dockertest startup.
    /// Each volume named is suffixed with the dockertest ID.
    /// This vector ONLY contains named_volumes and only their names, the container_path is stored
    /// in the Composition.
    named_volumes: Vec<String>,
    /// The docker network name to use for this test.
    /// This may be an existing, external network.
    network: String,
    /// ID of this DockerTest instance.
    /// When tests are run in parallel multiple DockerTest instances will exist at the same time,
    /// to distinguish which resources belongs to each test environment the resource name should be
    /// suffixed with this ID.
    /// This applies to resouces such as docker network names and named volumes.
    pub(crate) id: String,
}

/// The test body parameter provided in the [DockerTest::run] argument closure.
///
/// This object allows one to interact with the containers within the test environment.
#[derive(Clone)]
pub struct DockerOperations {
    /// A complete copy of the current engine under test.
    /// We _really_ wish to use a reference somehow here, but cannot easily do so due to
    /// lifetime conflicts. We may want to revisit this architecture decision in the future.
    engine: Engine<Orbiting>,
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
    /// Non-panicking version of [DockerOperations::handle].
    fn try_handle<'a>(&'a self, handle: &'a str) -> Result<&'a RunningContainer, DockerTestError> {
        if self.engine.handle_collision(handle) {
            return Err(DockerTestError::TestBody(format!(
                "handle '{}' defined multiple times",
                handle
            )));
        }

        self.engine.resolve_handle(handle).ok_or_else(|| {
            DockerTestError::TestBody(format!("container with handle '{}' not found", handle))
        })
    }

    /// Retrieve the `RunningContainer` identified by this handle.
    ///
    /// A container is identified within dockertest by its assigned or derived handler.
    /// If no explictly set through `set_handle`, the value will be equal to the
    /// repository name used when creating the container.
    ///
    /// # Panics
    /// This function panics if the requested handle does not exist, or there
    /// are conflicting containers with the same repository name is present without custom
    /// configured container names.
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

impl Runner {
    /// Creates a new DockerTest Runner.
    ///
    /// # Panics
    /// If a connection to the configured docker daemon cannot be established,
    /// this function panics. Use the [Runner::try_new] variant to catch the error.
    pub async fn new(config: DockerTest) -> Runner {
        Self::try_new(config).await.unwrap()
    }

    /// Creates a new DockerTest [Runner]. Returns error on Docker daemon connection failure.
    pub async fn try_new(config: DockerTest) -> Result<Runner, DockerTestError> {
        let client = Docker::new()?;
        let id = generate_random_string(20);

        let network = match &config.network {
            Network::External(n) => n.clone(),
            Network::Isolated => format!("dockertest-rs-{}", id),
            // The singular network is referenced by ID instead of name and therefore we can't know it
            // statically.
            // And since we need the network reference now, we need to create the network upfront instead of during `resolve_network'.
            Network::Singular => {
                SCOPED_NETWORKS
                    .create_singular_network(
                        &client,
                        own_container_id().as_deref(),
                        &config.namespace,
                    )
                    .await?
            }
        };

        Ok(Runner {
            client,
            named_volumes: Vec::new(),
            network,
            id,
            config,
        })
    }

    /// Internal impl of the public `run` method, to catch internal panics
    pub async fn run_impl<T, Fut>(mut self, test: T) -> Result<(), DockerTestError>
    where
        T: FnOnce(DockerOperations) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // If we are inside a container, we need to retrieve our container ID.
        self.check_if_inside_container();

        // Before constructing the compositions, we ensure that all configured
        // docker volumes have been created.
        self.resolve_named_volumes().await?;

        let compositions = std::mem::take(&mut self.config.compositions);
        let mut engine = bootstrap(compositions);
        engine.resolve_final_container_name(&self.config.namespace);

        let mut engine = engine.fuel();
        engine.resolve_inject_container_name_env()?;
        engine
            .pull_images(&self.client, &self.config.default_source)
            .await?;

        self.resolve_network().await?;

        // Create PendingContainers from the Compositions
        let engine = match engine
            .ignite(&self.client, &self.network, &self.config.network)
            .await
        {
            Ok(e) => e,
            Err(engine) => {
                let mut creation_failures = engine.creation_failures();
                let total = creation_failures.len();
                creation_failures.iter().enumerate().for_each(|(i, e)| {
                    trace!("container {} of {} creation failures: {}", i + 1, total, e);
                });

                let engine = engine.decommission();
                if let Err(errors) = engine.handle_startup_logs().await {
                    for err in errors {
                        error!("{err}");
                    }
                }
                self.teardown(engine, false).await;

                // QUESTION: What is the best option for us to propagate multiple errors?
                return Err(creation_failures
                    .pop()
                    .expect("dockertest bug: cleanup path expected container creation error"));
            }
        };

        // Ensure we drive all the waitfor conditions to completion when we start the containers
        let mut engine = match engine.orbiting().await {
            Ok(e) => e,
            Err((engine, e)) => {
                // Teardown everything on error
                let engine = engine.decommission();
                if let Err(errors) = engine.handle_startup_logs().await {
                    for err in errors {
                        error!("{err}");
                    }
                }
                self.teardown(engine, false).await;

                return Err(e);
            }
        };

        // When inspecting containers for their IP addresses the network key is the name of the
        // network and not the ID.
        // In a singular network configuation `self.network` will contain the ID of the the network
        // and not the name.
        // We do not suffer the ambigious network name problem here as we have already decided
        // which of the networks named `dockertest` to use and it will be the only network the
        // containers are connected to.
        let network_name = match self.config.network {
            Network::Singular => SCOPED_NETWORKS.name(&self.config.namespace),
            Network::External(_) | Network::Isolated => self.network.clone(),
        };

        // Run container inspection to get up-to-date runtime information
        if let Err(mut errors) = engine.inspect(&self.client, &network_name).await {
            let total = errors.len();
            errors.iter().enumerate().for_each(|(i, e)| {
                trace!("container {} of {} inspect failures: {}", i + 1, total, e);
            });

            // Teardown everything on error
            let engine = engine.decommission();
            self.teardown(engine, false).await;

            // QUESTION: What is the best option for us to propagate multiple errors?
            return Err(errors
                .pop()
                .expect("dockertest bug: cleanup path expected container inspect error"));
        };

        // We are ready to invoke the test body now
        let ops = DockerOperations {
            engine: engine.clone(),
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
                        Level::DEBUG,
                        "test body failed (cancelled: {}, panicked: {})",
                        e.is_cancelled(),
                        e.is_panic()
                    );
                    Err(e.try_into_panic().ok())
                }
            };

        let engine = engine.decommission();
        if let Err(errors) = engine.handle_logs(result.is_err()).await {
            for err in errors {
                error!("{err}");
            }
        }
        self.teardown(engine, result.is_err()).await;

        if let Err(option) = result {
            match option {
                Some(panic) => panic::resume_unwind(panic),
                None => panic!("test future cancelled"),
            }
        }

        Ok(())
    }

    /// Checks if we are inside a container, and if so sets our container ID.
    /// The user of dockertest is responsible for setting these env variables.
    fn check_if_inside_container(&mut self) {
        if let Some(id) = own_container_id() {
            event!(
                Level::TRACE,
                "dockertest container id env is set, we are running inside a container, id: {}",
                id
            );
            self.config.container_id = Some(id);
        } else {
            event!(
                Level::TRACE,
                "dockertest container id env is not set, running native on host"
            );
        }
    }

    async fn resolve_network(&self) -> Result<(), DockerTestError> {
        match &self.config.network {
            // Singular network is created during runner creation.
            // External network is created externally.
            Network::Singular | Network::External(_) => Ok(()),
            Network::Isolated => {
                self.client
                    .create_network(&self.network, self.config.container_id.as_deref())
                    .await
            }
        }
    }

    /// Teardown everything this test created, in accordance with the prune strategy.
    async fn teardown(&self, engine: Engine<Debris>, test_failed: bool) {
        // Ensure we cleanup static container regardless of prune strategy
        engine
            .disconnect_static_containers(&self.client, &self.network, &self.config.network)
            .await;

        match env_prune_strategy() {
            PruneStrategy::RunningRegardless => {
                event!(
                    Level::DEBUG,
                    "Leave all containers running regardless of outcome"
                );
            }

            PruneStrategy::RunningOnFailure if test_failed => {
                event!(
                    Level::DEBUG,
                    "Leaving all containers running due to test failure"
                );
            }

            // We only stop, and do not remove, if test failed and our strategy
            // tells us to do so.
            PruneStrategy::StopOnFailure if test_failed => {
                self.client
                    .stop_containers(engine.cleanup_containers())
                    .await;
                self.teardown_network().await;
            }

            // Catch all to remove everything.
            PruneStrategy::StopOnFailure
            | PruneStrategy::RunningOnFailure
            | PruneStrategy::RemoveRegardless => {
                event!(Level::DEBUG, "forcefully removing all containers");

                // Volumes have to be removed after the containers, as we will get a 409 from the
                // docker daemon if the volume is still in use by a container.
                // We therefore run the container remove futures to completion before trying to remove
                // volumes. We will not be able to remove volumes if the associated container was not
                // removed successfully.
                self.client
                    .remove_containers(engine.cleanup_containers())
                    .await;
                self.teardown_network().await;

                self.client.remove_volumes(&self.named_volumes).await;
            }
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
        self.config.compositions.iter_mut().for_each(|c| {
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

    async fn teardown_network(&self) {
        match self.config.network {
            // The singular network should never be deleted
            Network::Singular => (),
            Network::External(_) => (),
            Network::Isolated => {
                self.client
                    .delete_network(&self.network, self.config.container_id.as_deref())
                    .await
            }
        }
    }
}

fn own_container_id() -> Option<String> {
    std::env::var("DOCKERTEST_CONTAINER_ID_INJECT_TO_NETWORK").ok()
}

/// Resolve the current prune strategy, provided by the environment.
fn env_prune_strategy() -> PruneStrategy {
    match std::env::var_os("DOCKERTEST_PRUNE") {
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
    }
}
