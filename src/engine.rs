//! The meaty internals of executing a single test.

use crate::composition::{Composition, LogPolicy};
use crate::container::{
    CleanupContainer, CreatedContainer, OperationalContainer, PendingContainer,
    StaticExternalContainer,
};
use crate::docker::Docker;
use crate::static_container::STATIC_CONTAINERS;
use crate::utils::generate_random_string;
use crate::{DockerTestError, Network, Source, StartPolicy};

use futures::future::join_all;
use tokio::task::JoinHandle;
use tracing::{event, Level};

use std::collections::{hash_map::Entry, HashMap, HashSet};

/// The initial phase.
pub struct Bootstrapping {
    kept: Vec<Composition>,
}
/// The preparation phase.
pub struct Fueling {
    kept: Vec<Composition>,
}
/// The creating phase.
pub struct Igniting {
    kept: Vec<Transitional>,
}
/// The in-flight phase.
#[derive(Clone)]
pub struct Orbiting {
    kept: Vec<Transitional>,
}
/// The last phase.
pub struct Debris {
    kept: Vec<CleanupContainer>,
    external: Vec<StaticExternalContainer>,
}

/// The internal mechanism to separate the lifecycles of a container.
/// NOTE: Clone is only implemented to support Engine<Orbit> DockerOperation clone.
#[derive(Clone)]
enum Transitional {
    Pending(PendingContainer),
    Running(OperationalContainer),
    CreationFailure(DockerTestError),
    StaticExternal(StaticExternalContainer),
    Sentinel,
}

/// The purpose of the Keeper is to hold the reference to each Container throughout the test,
/// regardless of which transitionary state the container is in its lifecycle.
///
/// It also serves are our primary mechanism for resolving a handle name to the referenced
/// container object, when required.
#[derive(Clone)]
struct Keeper {
    /// If we have any handle collisions, they are registered here.
    /// Thus, if any reside here, they cannot be dynamically referenced.
    lookup_collisions: HashSet<String>,
    /// This map stores the mapping between a handle and its index into `kept`.
    lookup_handlers: HashMap<String, usize>,
}

// NOTE: Clone is only derived for Engine<Orbiting>, to delegate ownership into DockerOperations.
// We have some lifetime issues regardless of how we wish to solve it, as long as we spawn
// the task under test, which require the 'static lifetime.
#[derive(Clone)]
pub(crate) struct Engine<P> {
    keeper: Keeper,
    phase: P,
}

/// Create a new [Engine] in [Bootstrapping] phase.
pub(crate) fn bootstrap(compositions: Vec<Composition>) -> Engine<Bootstrapping> {
    let mut handlers: HashMap<String, usize> = HashMap::new();
    let mut collisions: HashSet<String> = HashSet::new();

    // NOTE: The insertion order is preserved.
    for (i, composition) in compositions.iter().enumerate() {
        let handle = composition.handle();

        if let Entry::Vacant(e) = handlers.entry(handle.clone()) {
            e.insert(i);
        } else {
            // Mark as collision key
            collisions.insert(handle);
        };
    }

    let keeper = Keeper {
        lookup_collisions: collisions,
        lookup_handlers: handlers,
    };

    Engine {
        keeper,
        phase: Bootstrapping { kept: compositions },
    }
}

impl Engine<Bootstrapping> {
    /// Perform the magic transformation info the final container name.
    pub fn resolve_final_container_name(&mut self, namespace: &str) {
        for c in self.phase.kept.iter_mut() {
            let suffix = generate_random_string(20);
            c.configure_container_name(namespace, &suffix);
        }
    }

    pub fn fuel(self) -> Engine<Fueling> {
        Engine::<Fueling> {
            keeper: self.keeper,
            phase: Fueling {
                kept: self.phase.kept,
            },
        }
    }
}

impl Engine<Fueling> {
    // TODO(REFACTOR): Create a type for the absurd (String, String, String) tuple
    pub fn resolve_inject_container_name_env(&mut self) -> Result<(), DockerTestError> {
        // Due to ownership issues, we must iterate once to verify that the handlers resolve
        // correctly, and thereafter we must apply the mutable changes to the env
        let mut composition_transforms: Vec<Vec<(String, String, String)>> = Vec::new();

        for c in self.phase.kept.iter() {
            let transformed: Result<Vec<(String, String, String)>, DockerTestError>
                = c.inject_container_name_env.iter().map(|(handle, env)| {
                // Guard against duplicate handle usage.
                if self.keeper.lookup_collisions.contains(handle) {
                    return Err(DockerTestError::Startup(format!("composition `{}` attempted to inject_container_name_env on duplicate handle `{}`", c.handle(), handle)));
                }

                // Resolve the handle
                let index: usize = match self.keeper.lookup_handlers.get(handle) {
                    Some(i) => *i,
                    None => return Err(DockerTestError::Startup(format!("composition `{}` attempted to inject_container_name_env on non-existent handle `{}`", c.handle(), handle))),
                };

                let container_name = self.phase.kept[index].container_name.clone();

                Ok((handle.clone(), container_name, env.clone()))
            }).collect();

            composition_transforms.push(transformed?);
        }

        for (index, c) in self.phase.kept.iter_mut().enumerate() {
            for (handle, name, env) in composition_transforms[index].iter() {
                // Inject the container name into env
                if let Some(old) = c.env.insert(env.to_string(), name.to_string()) {
                    event!(Level::WARN, "overwriting previously configured environment variable `{} = {}` with injected container name for handle `{}`", env, old, handle);
                }
            }
        }

        Ok(())
    }

    /// Pull the `Image` of all `Composition`s.
    ///
    /// This will ensure that all docker images is present on the local daemon
    /// and we are able to issue a create container operation.
    pub async fn pull_images(
        &self,
        client: &Docker,
        default: &Source,
    ) -> Result<(), DockerTestError> {
        let mut future_vec = Vec::new();

        // QUESTION: Can we not iter().map() this?
        for composition in self.phase.kept.iter() {
            let fut = client.pull_image(composition.image(), default);

            future_vec.push(fut);
        }

        join_all(future_vec).await;
        Ok(())
    }

    /// On error, the engine contains at least one container that failed to ignite.
    pub async fn ignite(
        self,
        client: &Docker,
        network: &str,
        network_settings: &Network,
    ) -> Result<Engine<Igniting>, Engine<Igniting>> {
        event!(Level::TRACE, "creating containers");

        // NOTE: The insertion order is preserved.
        // To achieve this, we need to keep all inserted compositions when they also represent
        // a static external container.
        let created: Vec<Result<CreatedContainer, DockerTestError>> = join_all(
            self.phase
                .kept
                .into_iter()
                .map(|c| client.create_container(c, Some(network), network_settings)),
        )
        .await;

        let mut startup_failure = false;
        let kept = created
            .into_iter()
            .map(|c| match c {
                Ok(c) => match c {
                    CreatedContainer::StaticExternal(e) => Transitional::StaticExternal(e),
                    CreatedContainer::Pending(p) => Transitional::Pending(p),
                },
                Err(e) => {
                    startup_failure = true;
                    Transitional::CreationFailure(e)
                }
            })
            .collect();

        let engine = Engine::<Igniting> {
            keeper: self.keeper,
            phase: Igniting { kept },
        };
        if startup_failure {
            Err(engine)
        } else {
            Ok(engine)
        }
    }
}

impl Engine<Igniting> {
    /// Move the engine forward into [Orbiting] phase.
    ///
    /// This will start and execute the relevant waitfor directives for each container.
    pub async fn orbiting(
        mut self,
    ) -> Result<Engine<Orbiting>, (Engine<Igniting>, DockerTestError)> {
        let result = self.start_containers().await;

        match result {
            Ok(_) => Ok(Engine::<Orbiting> {
                keeper: self.keeper,
                phase: Orbiting {
                    kept: self.phase.kept,
                },
            }),
            Err(e) => Err((self, e)),
        }
    }

    // TODO: Refactor to return Vec<DockerTestError> on Err
    async fn start_containers(&mut self) -> Result<(), DockerTestError> {
        // We clone out all our pending containers.
        // This will simplify alot of the gathering logic. We may be able to avoid this
        // clone in the future if we commit to changing the [WaitFor] signature.
        //
        // We manipulate the kept indices by correlating the ids to update with the running
        // transformed container.
        let pending = self.phase.kept.iter().flat_map(|t| match t {
            Transitional::Pending(p) => Some(p.clone()),
            _ => None,
        });

        let (relaxed, strict): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|c| c.start_policy == StartPolicy::Relaxed);

        // Asynchronously start all relaxed containers.
        let starting_relaxed = Self::start_relaxed_containers(relaxed);
        let strict_success = Self::start_strict_containers(strict).await?;
        let relaxed_success = Self::wait_for_relaxed_containers(starting_relaxed).await?;

        let mut containers = Vec::new();
        containers.extend(strict_success.into_iter());
        containers.extend(relaxed_success.into_iter());
        containers.extend(STATIC_CONTAINERS.external_containers().await.into_iter());

        // An important consideration herein is to maintain the same insertion order
        // of the original vector, when updating our Transitional::* variants.
        // This is due to the [Keeper] holding the handle -> indices lookup table,
        // which we must use to resolve the correct [OperationalContainer]
        for started in containers.into_iter() {
            // Locate the entry into kept of the started container
            let position = match self.phase.kept.iter().position(|x| match x {
                Transitional::Pending(p) => p.id == started.id,
                Transitional::StaticExternal(e) => e.handle == started.handle,
                _ => false,
            }) {
                Some(e) => e,
                None => continue,
            };

            // Create the [OperationalContainer] variant out of the pending
            let current = std::mem::replace(&mut self.phase.kept[position], Transitional::Sentinel);
            let running = match current {
                Transitional::Pending(_) | Transitional::StaticExternal(_) => {
                    Transitional::Running(started)
                }
                _ => continue,
            };

            self.phase.kept[position] = running;
        }

        Ok(())
    }

    // Implementation detail
    fn start_relaxed_containers(
        containers: Vec<PendingContainer>,
    ) -> Vec<JoinHandle<Result<OperationalContainer, DockerTestError>>> {
        event!(Level::TRACE, "starting relaxed containers");
        containers
            .into_iter()
            .map(|c| tokio::spawn(c.start()))
            .collect()
    }

    // Implementation detail
    // We currently only report the first error
    async fn start_strict_containers(
        pending: Vec<PendingContainer>,
    ) -> Result<Vec<OperationalContainer>, DockerTestError> {
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

    // Implementation detail
    async fn wait_for_relaxed_containers(
        starting_relaxed: Vec<JoinHandle<Result<OperationalContainer, DockerTestError>>>,
    ) -> Result<Vec<OperationalContainer>, DockerTestError> {
        let mut running_relaxed: Vec<OperationalContainer> = Vec::new();
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

    // QUESTION: Create a structured object with metadata from the composition for
    // the object representation a creation failure?
    pub fn creation_failures(&self) -> Vec<DockerTestError> {
        self.phase
            .kept
            .iter()
            .flat_map(|e| match e {
                Transitional::CreationFailure(err) => Some(err),
                _ => None,
            })
            .cloned()
            .collect()
    }

    /// Transforming the engine into one holding all debris containers
    /// we can teardown and handle.
    pub fn decommission(self) -> Engine<Debris> {
        let mut external = Vec::new();
        let kept = self
            .phase
            .kept
            .into_iter()
            .flat_map(|x| match x {
                Transitional::Running(r) => Some(r.into()),
                Transitional::Pending(r) => Some(r.into()),
                Transitional::StaticExternal(s) => {
                    external.push(s);
                    None
                }
                Transitional::Sentinel | Transitional::CreationFailure(_) => None,
            })
            .collect();

        Engine::<Debris> {
            keeper: self.keeper,
            phase: Debris { kept, external },
        }
    }
}

impl Engine<Orbiting> {
    pub fn decommission(self) -> Engine<Debris> {
        let mut external = Vec::new();
        let kept = self
            .phase
            .kept
            .into_iter()
            .flat_map(|x| match x {
                Transitional::Running(r) => Some(r.into()),
                Transitional::StaticExternal(r) => {
                    external.push(r);
                    None
                }
                _ => None,
            })
            .collect();

        Engine::<Debris> {
            keeper: self.keeper,
            phase: Debris { kept, external },
        }
    }

    /// Query whether or not the provided handle resolve to conflicting containers.
    pub fn handle_collision(&self, handle: &str) -> bool {
        self.keeper.lookup_collisions.contains(handle)
    }

    pub fn resolve_handle(&self, handle: &str) -> Option<&OperationalContainer> {
        let index = match self.keeper.lookup_handlers.get(handle) {
            None => return None,
            Some(i) => i,
        };

        match &self.phase.kept[*index] {
            Transitional::Running(r) => Some(r),
            // FIXME: report/handle multiple match arms
            _ => None,
        }
    }

    pub async fn inspect(
        &mut self,
        client: &Docker,
        network_name: &str,
    ) -> Result<(), Vec<DockerTestError>> {
        // TODO: Run the inspect operation in paralell with futures, and join_all
        // Need to figure out how to best update their state in their future.

        let mut errors = Vec::new();
        for transitional in self.phase.kept.iter_mut() {
            // Ensure that we have a OperationalContainer
            let container = match transitional {
                Transitional::Running(r) => r,
                // FIXME: We might have to report/handle each arm here
                _ => continue,
            };

            // On Windows container IPs cannot be resolved from outside a container.
            // So container IPs in the test body are useless and the only way to contact a
            // container is through a port map and localhost.
            // To avoid have users to have cfg!(windows) in their test bodies, we simply set all
            // container ips to localhost
            //
            // TODO: Find another strategy to contact containers from the test body on Windows.
            if cfg!(windows) {
                container.ip = std::net::Ipv4Addr::new(127, 0, 0, 1);
                continue;
            }

            match client
                .get_container_ip_and_ports(&container.id, network_name)
                .await
            {
                Ok(info) => {
                    container.ip = info.ip;
                    container.ports = info.ports;
                }
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl Engine<Debris> {
    /// Handle container logs during test execution.
    ///
    /// This function handles logs on per-container bases.
    pub async fn handle_logs(&self, test_failed: bool) -> Result<(), Vec<DockerTestError>> {
        let mut errors = vec![];

        for container in self.phase.kept.iter() {
            if let Some(log_options) = &container.log_options {
                let result = match log_options.policy {
                    LogPolicy::Always => {
                        container
                            .handle_log(&log_options.action, &log_options.source)
                            .await
                    }
                    LogPolicy::OnError => {
                        if !test_failed {
                            continue;
                        }
                        container
                            .handle_log(&log_options.action, &log_options.source)
                            .await
                    }
                    LogPolicy::OnStartupError => continue,
                };

                let result = result.map_err(|error| {
                    DockerTestError::LogWriteError(format!(
                        "unable to handle logs for: {}: {}",
                        container.name, error
                    ))
                });

                if let Err(err) = result {
                    errors.push(err);
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Handle container logs during startup.
    ///
    /// This function handles logs on per-container bases.
    pub async fn handle_startup_logs(&self) -> Result<(), Vec<DockerTestError>> {
        let mut errors = vec![];

        for container in self.phase.kept.iter() {
            if let Some(log_options) = &container.log_options {
                let result = container
                    .handle_log(&log_options.action, &log_options.source)
                    .await
                    .map_err(|error| {
                        DockerTestError::LogWriteError(format!(
                            "unable to handle logs for: {}: {}",
                            container.name, error
                        ))
                    });

                if let Err(err) = result {
                    errors.push(err);
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Ensure that our static containers are cleaned up individually.
    pub async fn disconnect_static_containers(
        &self,
        client: &Docker,
        network: &str,
        network_mode: &Network,
    ) {
        let mut static_cleanup: Vec<&str> = self
            .phase
            .kept
            .iter()
            .filter_map(|c| {
                if c.is_static() {
                    Some(c.id.as_str())
                } else {
                    None
                }
            })
            .collect();

        self.phase
            .external
            .iter()
            .for_each(|e| static_cleanup.push(e.id.as_str()));

        STATIC_CONTAINERS
            .cleanup(client, network, network_mode, static_cleanup)
            .await;
    }

    pub fn cleanup_containers(&self) -> &[CleanupContainer] {
        &self.phase.kept
    }
}
