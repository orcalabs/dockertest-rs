//! The main library structures.

use crate::container::RunningContainer;
use crate::engine::{bootstrap, Debris, Engine, Orbiting};
use crate::utils::{connect_with_local_or_tls_defaults, generate_random_string};
use crate::{DockerTest, DockerTestError};

use bollard::{
    network::{CreateNetworkOptions, DisconnectNetworkOptions},
    volume::RemoveVolumeOptions,
    Docker,
};
use futures::future::{join_all, Future};
use tracing::{event, trace, Level};

use std::any::Any;
use std::clone::Clone;
use std::collections::HashMap;
use std::panic;

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
    /// A handle for a [RunningContainer] will be either:
    /// a) the `repository` name for the [Image](crate::image::Image) when creating the `Composition`
    /// b) the container name configured on `Composition` [with_container_name].
    ///
    /// # Panics
    /// This function panics if the requested handle does not exist, or there
    /// are conflicting containers with the same repository name is present without custom
    /// configured container names.
    ///
    /// [with_container_name]: crate::Composition::with_container_name
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
    pub fn new(config: DockerTest) -> Runner {
        Self::try_new(config).unwrap()
    }

    /// Creates a new DockerTest [Runner]. Returns error on Docker daemon connection failure.
    pub fn try_new(config: DockerTest) -> Result<Runner, DockerTestError> {
        let client = connect_with_local_or_tls_defaults()?;
        let id = generate_random_string(20);
        Ok(Runner {
            client,
            named_volumes: Vec::new(),
            network: config
                .external_network
                .as_ref()
                .cloned()
                .unwrap_or_else(|| format!("dockertest-rs-{}", id)),
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

        // Create the network only if we have not been supplied an existing external network.
        if self.config.external_network.is_none() {
            self.create_network().await?;
        }

        // Create PendingContainers from the Compositions
        let engine = match engine
            .ignite(
                &self.client,
                &self.network,
                self.config.external_network.is_some(),
            )
            .await
        {
            Ok(e) => e,
            Err(engine) => {
                let mut cleanup = engine.cleanup().await?;
                let total = cleanup.len();
                cleanup.iter().enumerate().for_each(|(i, e)| {
                    trace!("container {} of {} creation failures: {}", i + 1, total, e);
                });
                // QUESTION: What is the best option for us to propagate multiple errors?
                return Err(cleanup
                    .pop()
                    .expect("dockertest bug: cleanup path expected container creation error"));
            }
        };

        // Ensure we drive all the waitfor conditions to completion when we start the containers
        let mut engine = match engine.orbiting().await {
            Ok(e) => e,
            Err((engine, e)) => {
                engine.cleanup().await?;
                return Err(e);
            }
        };

        // Run container inspection to get up-to-date runtime information
        if let Err(mut errors) = engine.inspect(&self.client, &self.network).await {
            let total = errors.len();
            errors.iter().enumerate().for_each(|(i, e)| {
                trace!("container {} of {} inspect failures: {}", i + 1, total, e);
            });
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

        engine.handle_logs(result.is_err()).await?;
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
        if let Ok(id) = std::env::var("DOCKERTEST_CONTAINER_ID_INJECT_TO_NETWORK") {
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

        if let Some(id) = self.config.container_id.clone() {
            if let Err(e) = self.add_self_to_network(id).await {
                if let Err(e) = self.client.remove_network(&self.network).await {
                    event!(
                        Level::ERROR,
                        "unable to remove docker network `{}`: {}",
                        self.network,
                        e
                    );
                }
                return Err(e);
            }
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

    /// Teardown everything this test created, in accordance with the prune strategy.
    async fn teardown(&self, engine: Engine<Debris>, test_failed: bool) {
        // Ensure we cleanup static container regardless of prune strategy
        engine
            .disconnect_static_containers(
                &self.client,
                &self.network,
                self.config.external_network.is_some(),
            )
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
                engine.stop_containers(&self.client).await;

                if self.config.external_network.is_none() {
                    self.teardown_network().await;
                }
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
                engine.remove_containers(&self.client).await;

                if self.config.external_network.is_none() {
                    self.teardown_network().await;
                }

                self.remove_volumes().await;
            }
        }
    }

    async fn remove_volumes(&self) {
        join_all(
            self.named_volumes
                .iter()
                .map(|v| {
                    event!(Level::INFO, "removing named volume: {:?}", &v);
                    let options = Some(RemoveVolumeOptions { force: true });
                    self.client.remove_volume(v, options)
                })
                .collect::<Vec<_>>(),
        )
        .await;
    }

    /// Make sure we remove the network we have previously created.
    async fn teardown_network(&self) {
        if let Some(id) = self.config.container_id.clone() {
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

    // Determines the final name for all named volumes, and modifies the Compositions accordingly.
    // Named volumes will have the following form: "USER_PROVIDED_VOLUME_NAME-DOCKERTEST_ID:PATH_IN_CONTAINER".
    async fn resolve_named_volumes(&mut self) -> Result<(), DockerTestError> {
        // Maps the original volume name to the suffixed ones
        // Key: "USER_PROVIDED_VOLUME_NAME"
        // Value: "USER_PROVIDED_VOLUME_NAME-DOCKERTEST_ID"
        let mut volume_name_map: HashMap<String, String> = HashMap::new();

        let suffix = self.id.clone();

        // Add the dockertest ID as a suffix to all named volume names.
        self.config.compositions.iter_mut().for_each(|mut c| {
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
