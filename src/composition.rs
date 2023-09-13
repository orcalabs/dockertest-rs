//! Represent a concrete instance of an Image, before it is ran as a Container.

use crate::container::{CreatedContainer, PendingContainer};
use crate::image::Image;
use crate::static_container::STATIC_CONTAINERS;
use crate::waitfor::{NoWait, WaitFor};
use crate::{DockerTestError, Network};

use bollard::{
    container::{
        Config, CreateContainerOptions, InspectContainerOptions, NetworkingConfig,
        RemoveContainerOptions,
    },
    models::HostConfig,
    service::{EndpointSettings, PortBinding},
    Docker,
};

use futures::future::TryFutureExt;
use std::collections::HashMap;
use tracing::{event, trace, Level};

/// Specifies the starting policy of a container specification.
///
/// - [StartPolicy::Strict] policy will enforce that the container is started in the order
///     it was added to [DockerTest].
/// - [StartPolicy::Relaxed] policy will not enforce any ordering,
///     all container specifications with a relaxed policy will be started concurrently.
///     These are all started asynchrously started before the strict policy containers
///     are started sequentially.
///
/// [DockerTest]: crate::DockerTest
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartPolicy {
    /// Concurrently start the Container with other Relaxed instances.
    Relaxed,
    /// Start Containers' sequentially in the order added to DockerTest.
    Strict,
}

/// Specifies who is responsible for managing a static container.
///
/// - [StaticManagementPolicy::External] indicates that the user is responsible for managing the
///     container, DockerTest will never start or remove/stop the container. The container will
///     be available through its handle in [DockerOperations]. If no external network is
///     supplied, the test-scoped network will be added to the external network, and subsequently
///     removed once the test terminates.
///     The externally managed container is assumed to be in a running state when the test starts.
///     If DockerTest cannot locate the the container, the test will fail.
/// - [StaticManagementPolicy::Internal] indicates that DockerTest will handle the lifecycle of
///     the container between all DockerTest instances within the test binary.
/// - [StaticManagementPolicy::Dynamic] indicates that DockerTest will start the
///     container if it does not already exists and will not clean it up. This way the same
///     container can be re-used across multiple `cargo test` invocations.
///     If the `DOCKERTEST_DYNAMIC` environment variable is set to `INTERNAL` or `EXTERNAL`, the management policy
///     will instead be set accordingly (either [StaticManagementPolicy::Internal] or [StaticManagementPolicy::External].
///     The purpose of this is to facilitate running tests locally and in CI/CD pipelines without having to alter management policies.
///     If a container already exists in a non-running state with the same name as a container with this policy, the startup
///     procedure will fail.
///
/// [DockerOperations]: crate::DockerOperations
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StaticManagementPolicy {
    /// The lifecycle of the container is managed by the user.
    External,
    /// DockerTest handles the lifecycle of the container.
    Internal,
    /// DockerTest starts the container if it does not exist and does not remove it, and will
    /// re-use the container across `cargo test` invocations.
    Dynamic,
}

/// Specifies how should dockertest should handle log output from this container.
#[derive(Clone, Debug)]
pub enum LogAction {
    /// Forward all outputs to their respective output sources of the dockertest process.
    Forward,
    /// Forward [LogSource] outputs to a specified file.
    ForwardToFile {
        /// The filepath to output to.
        path: String,
    },
    /// Forward [LogSource] outputs to stdout of the dockertest process.
    ForwardToStdOut,
    /// Forward [LogSource] outputs to stderr of the dockertest process.
    ForwardToStdErr,
}

/// Specifies which log sources we want to read from containers.
#[derive(Clone, Debug)]
pub enum LogSource {
    /// Read stderr only.
    StdErr,
    /// Read stdout only.
    StdOut,
    /// Read stdout and stderr.
    Both,
}

/// Specifies when [LogAction] is applicable.
#[derive(Clone, Debug)]
pub enum LogPolicy {
    /// [LogAction] is always applicable.
    Always,
    /// [LogAction] is applicable only if an error occures.
    OnError,
    /// [LogAction] is applicable only if a startup error occures.
    OnStartupError,
}

/// Specifies how dockertest should handle logging output from this specific container.
#[derive(Clone, Debug)]
pub struct LogOptions {
    /// The logging actions to be performed.
    pub action: LogAction,
    /// Under which conditions should we perform the log actions?
    pub policy: LogPolicy,
    /// Specifies log sources we want to read from container.
    pub source: LogSource,
}

impl Default for LogOptions {
    fn default() -> LogOptions {
        LogOptions {
            action: LogAction::Forward,
            policy: LogPolicy::OnError,
            source: LogSource::StdErr,
        }
    }
}

/// Represents an instance of an [Image].
///
/// The Composition is used to specialize an image whose name, version, tag and source is known,
/// but before one can create a [crate::container:: RunningContainer] from an image,
/// it must be augmented with information about how to start it, how to ensure it has been
/// started, environment variables and runtime commands.
/// Thus, this structure represents the concrete instance of an [Image] that will be started
/// and become a [crate::container::RunningContainer].
///
/// NOTE: This is an internal implementation detail. This used to be a public interface.
#[derive(Clone, Debug)]
pub struct Composition {
    /// User provided name of the container.
    ///
    /// This will dictate the final container_name and the container_handle_key of the container
    /// that will be created from this Composition.
    user_provided_container_name: Option<String>,

    /// Network aliases for the container.
    network_aliases: Option<Vec<String>>,

    /// The name of the container to be created by this Composition.
    ///
    /// When the composition is created, this field defaults to the repository name of the
    /// associated image. If the user provides an alternative container name, this will be stored
    /// in its own dedicated field.
    ///
    /// The final format of the container name we will create will be on the following format:
    /// `{namespace}-{name}-{suffix}` where
    /// - `{namespace}` is the configured namespace with [crate::DockerTest].
    /// - `{name}` is either the user provided container name, or this default value.
    /// - `{suffix}` randomly generated pattern.
    pub(crate) container_name: String,

    /// A trait object holding the implementation that indicate container readiness.
    wait: Box<dyn WaitFor>,

    /// The environmentable variables that will be passed to the container.
    pub(crate) env: HashMap<String, String>,

    /// The command to pass to the container.
    cmd: Vec<String>,

    /// The start policy of this container, codifing the inter-depdencies between containers.
    start_policy: StartPolicy,

    /// The base image that will be the container we will be starting.
    image: Image,

    /// Named volumes associated with this composition, are in the form of:
    /// - "(VOLUME_NAME,CONTAINER_PATH)"
    pub(crate) named_volumes: Vec<(String, String)>,

    /// Final form of named volume names.
    ///
    /// DockerTest is responsible for constructing the final names and adding them to this vector.
    /// The final name will be on the form `VOLUME_NAME-RANDOM_SUFFIX/CONTAINER_PATH`.
    pub(crate) final_named_volume_names: Vec<String>,

    /// Bind mounts associated with this composition, are in the form of:
    /// - `HOST_PATH:CONTAINER_PATH`
    ///
    /// NOTE: As bind mounts do not outlive the container they are mounted in they do not need to
    /// be cleaned up.
    bind_mounts: Vec<String>,

    /// All user specified container name injections as environment variables.
    /// Tuple contains (handle, env).
    pub(crate) inject_container_name_env: Vec<(String, String)>,

    /// Port mapping (used for Windows-compatibility)
    port: Vec<(String, String)>,

    /// Allocates an ephemeral host port for all of a container’s exposed ports.
    ///
    /// Port forwarding is useful on operating systems where there is no network connectivity
    /// between system and the Docker Desktop VM.
    pub(crate) publish_all_ports: bool,

    /// Who is responsible for managing the lifecycle of the container.
    ///
    /// A composition can be marked as static, where the lifecycle of the container outlives
    /// the individual test.
    management: Option<StaticManagementPolicy>,

    /// Logging options for this specific container.
    pub(crate) log_options: Option<LogOptions>,

    /// Whether this composition should be started in privileged mode.
    /// Privileged mode is required for some images, such as the `docker:dind` image.
    /// See https://docs.docker.com/engine/reference/run/#runtime-privilege-and-linux-capabilities
    /// for more information.
    /// Defaults to false.
    /// NOTE: This is only supported on Linux.
    /// NOTE: This is only supported on Docker 1.13 and above.
    /// NOTE: This is only supported on Docker API 1.25 and above.
    /// NOTE: This is only supported on Docker Engine 1.13 and above.
    pub(crate) privileged: bool,
}

impl Composition {
    /// Creates a [Composition] based on the [Image] repository name provided.
    ///
    /// This will internally create the [Image] based on the provided repository name,
    /// and default the tag to `latest`.
    ///
    /// This is the shortcut method of constructing a [Composition].
    /// See [with_image](Composition::with_image) to create one with a provided [Image].
    pub fn with_repository<T: ToString>(repository: T) -> Composition {
        let copy = repository.to_string();
        Composition {
            user_provided_container_name: None,
            network_aliases: None,
            image: Image::with_repository(&copy),
            container_name: copy.replace('/', "-"),
            wait: Box::new(NoWait {}),
            env: HashMap::new(),
            cmd: Vec::new(),
            start_policy: StartPolicy::Relaxed,
            bind_mounts: Vec::new(),
            named_volumes: Vec::new(),
            inject_container_name_env: Vec::new(),
            final_named_volume_names: Vec::new(),
            port: Vec::new(),
            publish_all_ports: false,
            management: None,
            log_options: Some(LogOptions::default()),
            privileged: false,
        }
    }

    /// Creates a [Composition] with the provided [Image].
    ///
    /// This is the long-winded way of defining a [Composition].
    /// See [with_repository](Composition::with_repository) for the shortcut method.
    pub fn with_image(image: Image) -> Composition {
        Composition {
            user_provided_container_name: None,
            network_aliases: None,
            container_name: image.repository().to_string().replace('/', "-"),
            image,
            wait: Box::new(NoWait {}),
            env: HashMap::new(),
            cmd: Vec::new(),
            start_policy: StartPolicy::Relaxed,
            bind_mounts: Vec::new(),
            named_volumes: Vec::new(),
            inject_container_name_env: Vec::new(),
            final_named_volume_names: Vec::new(),
            port: Vec::new(),
            publish_all_ports: false,
            management: None,
            log_options: Some(LogOptions::default()),
            privileged: false,
        }
    }

    /// Sets the [StartPolicy] for this [Composition].
    ///
    /// Defaults to a [relaxed](StartPolicy::Relaxed) policy.
    pub fn with_start_policy(self, start_policy: StartPolicy) -> Composition {
        Composition {
            start_policy,
            ..self
        }
    }

    /// Assigns the full set of environmental variables available for the [RunningContainer].
    ///
    /// Each key in the map should be the environmental variable name
    /// and its corresponding value will be set as its value.
    ///
    /// This method replaces the entire existing env map provided.
    ///
    /// [RunningContainer]: crate::container::RunningContainer
    pub fn with_env(self, env: HashMap<String, String>) -> Composition {
        Composition { env, ..self }
    }

    /// Sets the command of the container.
    ///
    /// If no entries in the command vector is provided to the [Composition],
    /// the command within the [Image] will be used, if any.
    pub fn with_cmd(self, cmd: Vec<String>) -> Composition {
        Composition { cmd, ..self }
    }

    /// Add a host port mapping to the container.
    ///
    /// This is useful when the host environment running docker cannot support IP routing
    /// within the docker network, such that test containers cannot communicate between themselves.
    /// This escape hatch allows the host to be involved to route traffic.
    /// This mechanism is not recommended, as concurrent tests utilizing the same host port
    /// will fail since the port is already in use.
    /// It is recommended to use [Composition::publish_all_ports].
    ///
    /// If an port mapping on the exported port has already been issued on the [Composition],
    /// it will be overidden.
    pub fn port_map(&mut self, exported: u32, host: u32) -> &mut Composition {
        self.port
            .push((format!("{}/tcp", exported), format!("{}", host)));
        self
    }

    /// Allocates an ephemeral host port for all of the container's exposed ports.
    ///
    /// Mapped host ports can be found via [crate::container::RunningContainer::host_port] method.
    pub fn publish_all_ports(&mut self, publish: bool) -> &mut Composition {
        self.publish_all_ports = publish;
        self
    }

    /// Sets the name of the container that will eventually be started.
    ///
    /// This is merely part of the final container name, and the full container name issued
    /// to docker will be generated.
    /// The container name assigned here is also used to resolve the `handle` concept used
    /// by dockertest.
    ///
    /// The container name defaults to the repository name.
    ///
    /// NOTE: If the `Composition` is a static container with an
    /// `External` management policy the container name *MUST* match the container_name of
    /// the external container and is required to be set.
    pub fn with_container_name<T: ToString>(self, container_name: T) -> Composition {
        Composition {
            user_provided_container_name: Some(container_name.to_string()),
            ..self
        }
    }

    /// Sets network aliases for this `Composition`.
    pub fn with_alias(self, aliases: Vec<String>) -> Composition {
        Composition {
            network_aliases: Some(aliases),
            ..self
        }
    }

    /// Adds network alias to this `Composition`
    pub fn alias(&mut self, alias: String) -> &mut Composition {
        match self.network_aliases {
            Some(ref mut network_aliases) => network_aliases.push(alias),
            None => self.network_aliases = Some(vec![alias]),
        };
        self
    }

    /// Sets the `WaitFor` trait object for this `Composition`.
    ///
    /// The default `WaitFor` implementation used is [RunningWait].
    ///
    /// [RunningWait]: crate::waitfor::RunningWait
    pub fn with_wait_for(self, wait: Box<dyn WaitFor>) -> Composition {
        Composition { wait, ..self }
    }

    /// Sets log options for this `Composition`.
    /// By default `LogAction::Forward`, `LogPolicy::OnError`, and `LogSource::StdErr` is enabled.
    /// To clear default log option pass `None` or specify your own log options.
    pub fn with_log_options(self, log_options: Option<LogOptions>) -> Composition {
        Composition {
            log_options,
            ..self
        }
    }

    /// Sets the environment variable to the given value.
    ///
    /// NOTE: if [with_env] is called after a call to [env], all values added by [env] will be overwritten.
    ///
    /// [env]: Composition::env
    /// [with_env]: Composition::with_env
    pub fn env<T: ToString, S: ToString>(&mut self, name: T, value: S) -> &mut Composition {
        self.env.insert(name.to_string(), value.to_string());
        self
    }

    /// Appends the command string to the current command vector.
    ///
    /// If no entries in the command vector is provided to the [Composition],
    /// the command within the [Image] will be used, if any.
    ///
    /// NOTE: if [with_cmd] is called after a call to [cmd], all entries to the command vector
    /// added with [cmd] will be overwritten.
    ///
    /// [cmd]: Composition::cmd
    /// [with_cmd]: Composition::with_cmd
    pub fn cmd<T: ToString>(&mut self, cmd: T) -> &mut Composition {
        self.cmd.push(cmd.to_string());
        self
    }

    /// Adds the given named volume to the Composition.
    /// Named volumes can be shared between containers, specifying the same named volume for
    /// another Composition will give both access to the volume.
    /// `path_in_container` has to be an absolute path.
    pub fn named_volume<T: ToString, S: ToString>(
        &mut self,
        volume_name: T,
        path_in_container: S,
    ) -> &mut Composition {
        self.named_volumes
            .push((volume_name.to_string(), path_in_container.to_string()));
        self
    }
    /// Adds the given bind mount to the Composition.
    /// A bind mount only exists for a single container and maps a given file or directory from the
    /// host to the container.
    /// Use named volumes if you want to share data between containers.
    /// The `host_path` can either point to a directory or a file that MUST exist on the local host.
    /// `path_in_container` has to be an absolute path.
    pub fn bind_mount<T: ToString, S: ToString>(
        &mut self,
        host_path: T,
        path_in_container: S,
    ) -> &mut Composition {
        // The ':Z' is needed due to permission issues, see
        // https://stackoverflow.com/questions/24288616/permission-denied-on-accessing-host-directory-in-docker
        // for more details
        self.bind_mounts.push(format!(
            "{}:{}:Z",
            host_path.to_string(),
            path_in_container.to_string()
        ));
        self
    }

    /// Inject the generated container name identified by `handle` into
    /// this Composition environment variable `env`.
    ///
    /// This is used to establish inter communication between running containers
    /// controlled by dockertest. This is traditionally established through environment variables
    /// for connection details, and thus the DNS resolving capabilities within docker will
    /// map the container name into the correct IP address.
    ///
    /// To correctly use this feature, the `StartPolicy` between the dependant containers
    /// must be configured such that these connections can successfully be established.
    /// Dockertest will not make any attempt to verify the integrity of these dependencies.
    pub fn inject_container_name<T: ToString, E: ToString>(
        &mut self,
        handle: T,
        env: E,
    ) -> &mut Composition {
        self.inject_container_name_env
            .push((handle.to_string(), env.to_string()));
        self
    }

    /// Defines this as a static container which will will only be cleaned up after the full test
    /// binary has executed.
    /// If the static container is used across multiple tests in the same test binary, Dockertest can only guarantee that
    /// the container will be started in its designated start order or earlier as other tests might
    /// have already started it.
    /// The container_name *MUST* be set to a unique value when using static containers.
    /// To refer to the same container across `Dockertest` instances set the same container name for the
    /// compostions.
    ///
    /// NOTE: When the `External` management policy is used, the container_name must be set to the
    /// name of the external container.
    pub fn static_container(&mut self, management: StaticManagementPolicy) -> &mut Composition {
        let management = match management {
            StaticManagementPolicy::External | StaticManagementPolicy::Internal => management,
            StaticManagementPolicy::Dynamic => match std::env::var("DOCKERTEST_DYNAMIC") {
                Ok(val) => match val.as_str() {
                    "EXTERNAL" => StaticManagementPolicy::External,
                    "INTERNAL" => StaticManagementPolicy::Internal,
                    "DYNAMIC" => StaticManagementPolicy::Dynamic,
                    _ => {
                        event!(Level::WARN, "DOCKERTEST_DYNAMIC environment variable set to unknown value, defaulting to Dynamic policy");
                        StaticManagementPolicy::Dynamic
                    }
                },
                Err(_) => management,
            },
        };
        self.management = Some(management);
        self
    }

    /// Should this container be started with priviledged mode enabled?
    /// This is required for some containers to run correctly.
    /// See https://docs.docker.com/engine/reference/run/#runtime-privilege-and-linux-capabilities
    /// for more details.
    pub fn privileged(&mut self) -> &mut Composition {
        self.privileged = true;
        self
    }

    /// Fetch the assigned [StaticManagementPolicy], if any.
    pub(crate) fn static_management_policy(&self) -> &Option<StaticManagementPolicy> {
        &self.management
    }

    /// Query whether this Composition should be handled through static container checks.
    fn is_static(&self) -> bool {
        self.management.is_some()
    }

    // Configure the container's name with the given namespace as prefix
    // and suffix.
    // We do this to ensure that we do not have overlapping container names
    // and make it clear which containers are run by DockerTest.
    pub(crate) fn configure_container_name(&mut self, namespace: &str, suffix: &str) {
        let name = match &self.user_provided_container_name {
            None => self.image.repository(),
            Some(n) => n,
        };

        if !self.is_static() {
            // The docker daemon does not like '/' or '\' in container names
            let stripped_name = name.replace('/', "_");

            self.container_name = format!("{}-{}-{}", namespace, stripped_name, suffix);
        } else {
            self.container_name = name.to_string();
        }
    }

    /// TODO: Refactor what is returned when creating the static container.
    pub(crate) async fn create(
        self,
        client: &Docker,
        network: Option<&str>,
        network_settings: &Network,
    ) -> Result<CreatedContainer, DockerTestError> {
        trace!("evaluating composition: {self:#?}");
        if self.is_static() {
            STATIC_CONTAINERS
                .create(self, client, network, network_settings)
                .await
        } else {
            self.create_inner(client, network)
                .await
                .map(CreatedContainer::Pending)
        }
    }

    // Performs container creation, should NOT be called outside of this module or the static
    // module.
    // This is only exposed such that the static module can reach it.
    pub(crate) async fn create_inner(
        self,
        client: &Docker,
        network: Option<&str>,
    ) -> Result<PendingContainer, DockerTestError> {
        event!(Level::DEBUG, "creating container: {}", self.container_name);

        let start_policy_clone = self.start_policy.clone();
        let container_name_clone = self.container_name.clone();

        if !self.is_static() {
            // Ensure we can remove the previous container instance, if it somehow still exists.
            // Only bail on non-recoverable failure.
            match remove_container_if_exists(client, &self.container_name).await {
                Ok(_) => {}
                Err(e) => match e {
                    DockerTestError::Recoverable(_) => {}
                    _ => return Err(e),
                },
            }
        }

        let image_id = self.image.retrieved_id();
        // Additional programming guard.
        // This Composition cannot be created without an image id, which
        // is set through `Image::pull`
        if image_id.is_empty() {
            return Err(DockerTestError::Processing("`Composition::create()` invoked without populating its image through `Image::pull()`".to_string()));
        }

        // As we can't return temporary values owned by this closure
        // we have to first convert our map into a vector of owned strings,
        // then convert it to a vector of borrowed strings (&str).
        // There is probably a better way to do this...
        let envs: Vec<String> = self
            .env
            .iter()
            .map(|(key, value)| format!("{}={}", key, value))
            .collect();
        let envs = envs.iter().map(|s| s.as_ref()).collect();
        let cmds = self.cmd.iter().map(|s| s.as_ref()).collect();

        let mut volumes: Vec<String> = Vec::new();
        for v in self.bind_mounts.iter() {
            event!(
                Level::DEBUG,
                "creating host_mounted_volume: {} for container {}",
                v.as_str(),
                self.container_name
            );
            volumes.push(v.to_string());
        }

        for v in self.final_named_volume_names.iter() {
            event!(
                Level::DEBUG,
                "creating named_volume: {} for container {}",
                &v,
                self.container_name
            );
            volumes.push(v.to_string());
        }

        let mut port_map: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        let mut exposed_ports: HashMap<&str, HashMap<(), ()>> = HashMap::new();

        for (exposed, host) in &self.port {
            let dest_port: Vec<PortBinding> = vec![PortBinding {
                host_ip: Some("127.0.0.1".to_string()),
                host_port: Some(host.clone()),
            }];
            port_map.insert(exposed.to_string(), Some(dest_port));
            exposed_ports.insert(exposed, HashMap::new());
        }

        let network_aliases = self.network_aliases.as_ref();
        let mut net_config = None;

        // Construct host config
        let host_config = network.map(|n| HostConfig {
            network_mode: Some(n.to_string()),
            binds: Some(volumes),
            port_bindings: Some(port_map),
            publish_all_ports: Some(self.publish_all_ports),
            privileged: Some(self.privileged),
            ..Default::default()
        });

        if let Some(n) = network {
            net_config = network_aliases.map(|a| {
                let mut endpoints = HashMap::new();
                let settings = EndpointSettings {
                    aliases: Some(a.to_vec()),
                    ..Default::default()
                };
                endpoints.insert(n, settings);
                NetworkingConfig {
                    endpoints_config: endpoints,
                }
            });
        }

        // Construct options for create container
        let options = Some(CreateContainerOptions {
            name: &self.container_name,
            // Sets the platform of the server if its multi-platform capable, we might support user
            // provided values here at a later time.
            platform: None,
        });

        let config = Config::<&str> {
            image: Some(&image_id),
            cmd: Some(cmds),
            env: Some(envs),
            networking_config: net_config,
            host_config,
            exposed_ports: Some(exposed_ports),
            ..Default::default()
        };

        trace!("creating container from options: {options:#?}, config: {config:#?}");

        let container_info = client
            .create_container(options, config)
            .map_err(|e| DockerTestError::Daemon(format!("failed to create container: {}", e)))
            .await?;

        let static_management_policy = self.static_management_policy().clone();
        Ok(PendingContainer::new(
            &container_name_clone,
            container_info.id,
            self.handle(),
            start_policy_clone,
            self.wait,
            client.clone(),
            static_management_policy,
            self.log_options.clone(),
        ))
    }

    // Returns the Image associated with this Composition.
    pub(crate) fn image(&self) -> &Image {
        &self.image
    }

    /// Retrieve a copy of the applicable handle name for this composition.
    ///
    /// NOTE: this value will be outdated if [Composition::with_container_name] is invoked
    /// with a different name.
    pub fn handle(&self) -> String {
        match &self.user_provided_container_name {
            None => self.image.repository().to_string(),
            Some(n) => n.clone(),
        }
    }
}

// Forcefully removes the given container if it exists.
async fn remove_container_if_exists(client: &Docker, name: &str) -> Result<(), DockerTestError> {
    client
        .inspect_container(name, None::<InspectContainerOptions>)
        .map_err(|e| DockerTestError::Recoverable(format!("container did not exist: {}", e)))
        .await?;

    // We were able to inspect it successfully, it exists.
    // Therefore, we can simply force remove it.
    let options = Some(RemoveContainerOptions {
        force: true,
        ..Default::default()
    });
    client
        .remove_container(name, options)
        .map_err(|e| DockerTestError::Daemon(format!("failed to remove existing container: {}", e)))
        .await
}

#[cfg(test)]
mod tests {
    use crate::composition::{remove_container_if_exists, Composition, StartPolicy};
    use crate::image::{Image, Source};
    use crate::utils::connect_with_local_or_tls_defaults;
    use crate::{DockerTestError, Network};

    use std::collections::HashMap;

    // Tests that the with_repository constructor creates
    // a Composition with the correct values
    #[test]
    fn test_with_repository_constructor() {
        let repository = "this_is_a_repository".to_string();

        let instance = Composition::with_repository(&repository);
        assert_eq!(
            repository,
            instance.image.repository(),
            "repository is not set to the correct value"
        );
        assert_eq!(
            repository, instance.container_name,
            "container_name should default to the repository"
        );
        assert_eq!(
            instance.env.len(),
            0,
            "there should be no environmental variables after constructing a Composition"
        );
        assert_eq!(
            instance.cmd.len(),
            0,
            "there should be no commands after constructing a Composition"
        );

        let equal = matches!(instance.start_policy, StartPolicy::Relaxed);
        assert!(equal, "start_policy should default to relaxed");
    }

    // Tests that the with_image constructor creates
    // a Composition with the correct values
    #[test]
    fn test_with_image_constructor() {
        let repository = "this_is_a_repository".to_string();
        let image = Image::with_repository(&repository);

        let instance = Composition::with_image(image);
        assert_eq!(
            repository,
            instance.image.repository(),
            "repository is not set to the correct value"
        );
        assert_eq!(
            repository, instance.container_name,
            "container_name should default to the repository"
        );
        assert_eq!(
            instance.env.len(),
            0,
            "there should be no environmental variables after constructing a Composition"
        );
        assert_eq!(
            instance.cmd.len(),
            0,
            "there should be no commands after constructing a Composition"
        );

        let equal = matches!(instance.start_policy, StartPolicy::Relaxed);
        assert!(equal, "start_policy should default to relaxed");
    }

    // Tests all methods that consumes the Composition
    // and mutates a field
    #[test]
    fn test_mutators() {
        let mut env = HashMap::new();

        let env_variable = "GOPATH".to_string();
        let env_value = "/home/kim/unsafe".to_string();

        env.insert(env_variable, env_value);
        let expected_env = env.clone();

        let cmd = "this_is_a_command".to_string();
        let cmds = vec![cmd];

        let expected_cmds = cmds.clone();

        let repository = "this_is_a_repository".to_string();

        let container_name = "this_is_a_container_name";

        let instance = Composition::with_repository(repository)
            .with_start_policy(StartPolicy::Strict)
            .with_env(env)
            .with_cmd(cmds)
            .with_container_name(container_name);

        let equal = matches!(instance.start_policy, StartPolicy::Strict);

        assert!(equal, "start_policy was not changed after invoking mutator");
        assert_eq!(
            expected_env, instance.env,
            "environmental variables not set correctly"
        );

        assert_eq!(expected_cmds, instance.cmd, "commands not set correctly");

        let correct_container_name = match instance.user_provided_container_name {
            Some(n) => n == container_name,
            None => false,
        };

        assert!(correct_container_name, "container_name not set correctly");
    }

    // Tests that the env method succesfully
    // adds the given environment variable to the Composition
    #[test]
    fn test_add_env() {
        let env_variable = "this_is_an_env_var".to_string();
        let env_value = "this_is_an_env_value".to_string();

        let repository = "this_is_a_repository".to_string();
        let mut instance = Composition::with_repository(repository);

        instance.env(env_variable.clone(), env_value.clone());

        assert_eq!(
            *instance
                .env
                .get(&env_variable)
                .expect("failed to get value from map that should be there"),
            env_value,
            "environmental variable not added correctly"
        );
    }

    // Tests that the cmd method succesfully
    // adds the given command to the Composition
    #[test]
    fn test_add_cmd() {
        let cmd = "this_is_a_command".to_string();
        let expected_cmd = vec![cmd.clone()];

        let repository = "this_is_a_repository".to_string();
        let mut instance = Composition::with_repository(repository);

        instance.cmd(cmd);

        assert_eq!(
            instance.cmd, expected_cmd,
            "command value not added correctly"
        );
    }

    /// Tests that we cannot create a container from a non-existent local repository image.
    #[tokio::test]
    async fn test_create_with_non_existing_local_image() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = "dockertest_create_with_non_existing_local_image";
        let composition = Composition::with_repository(repository);

        // Invoking image pull to populate the image id should err
        // TODO: assert a proper error message
        assert!(composition
            .image
            .pull(&client, &Source::Local)
            .await
            .is_err());

        // This will then fail due to missing image id
        let result = composition.create(&client, None, &Network::Isolated).await;
        // TODO: assert a proper error message
        assert!(
            result.is_err(),
            "should fail to start a Composition with non-exisiting image"
        );
    }

    /// Check that a simple composition from repository can be successfully created.
    #[tokio::test]
    async fn test_simple_create_composition_from_repository_success() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = "dockertest-rs/hello";
        let mut composition = Composition::with_repository(repository);
        composition.container_name =
            "test_simple_create_composition_from_repository_success".to_string();

        // ensure image metadata is populated (through pull infrastructure)
        composition
            .image
            .pull(&client, &Source::Local)
            .await
            .unwrap();

        let result = composition.create(&client, None, &Network::Isolated).await;
        assert!(
            result.is_ok(),
            "failed to start Composition: {}",
            result.err().unwrap()
        );
    }

    /// Tests that two consecutive Compositions creating a container with the exact same
    /// `container_name` will still be allowed to be created.
    ///
    /// The expected behaviour is thus to remove the old container
    /// (since we assume it will be an _old_ name collision).
    #[tokio::test]
    async fn test_create_with_existing_container() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = "dockertest-rs/hello";
        let container_name = "dockertest_create_with_existing_container".to_string();

        let mut composition1 = Composition::with_repository(repository);
        // configure the `container_name` inline instead of through `with_container_name`,
        // to avoid the user provided container name logic.
        composition1.container_name = container_name;
        // ensure image metadata is populated (through pull infrastructure)
        composition1
            .image
            .pull(&client, &Source::Local)
            .await
            .unwrap();

        let composition2 = composition1.clone();

        // Initial setup - first container that already exists.
        let result = composition1.create(&client, None, &Network::Isolated).await;
        assert!(
            result.is_ok(),
            "failed to start first composition: {}",
            result.err().unwrap()
        );

        // Creating a second one should still be allowed, since we expect the first one
        // to be removed.
        let result = composition2.create(&client, None, &Network::Isolated).await;
        assert!(
            result.is_ok(),
            "failed to start second composition: {}",
            result.err().unwrap()
        );
    }

    /// Tests the `remove_container_if_exists` method when container exists.
    #[tokio::test]
    async fn test_remove_existing_container() {
        let client = connect_with_local_or_tls_defaults().unwrap();
        let repository = "dockertest-rs/hello";
        let container_name = "dockertest_remove_existing_container_test_name";
        let mut composition = Composition::with_repository(repository);
        composition.container_name = container_name.to_string();

        // ensure image metadata is populated (through pull infrastructure)
        composition
            .image
            .pull(&client, &Source::Local)
            .await
            .unwrap();

        // Create out composition
        let result = composition.create(&client, None, &Network::Isolated).await;
        assert!(
            result.is_ok(),
            "failed to start composition: {}",
            result.err().unwrap()
        );

        // Remove it by name
        let result = remove_container_if_exists(&client, container_name).await;
        assert!(
            result.is_ok(),
            "failed to remove existing container: {}",
            result.unwrap_err()
        );
    }

    /// Tests that we fail when trying to remove a non-existing container through
    /// `remove_container_if_exists`.
    #[tokio::test]
    async fn test_remove_non_existing_container() {
        let client = connect_with_local_or_tls_defaults().unwrap();

        let result = remove_container_if_exists(&client, "dockertest_non_existing_container").await;

        let res = match result {
            Ok(_) => false,
            Err(e) => matches!(e, DockerTestError::Recoverable(_)),
        };
        assert!(res, "should fail to remove non-existing container");
    }

    // Tests that the configurate_container_name method correctly sets the Composition's
    // container_name when the user has not specified a container_name
    #[test]
    fn test_configurate_container_name_without_user_supplied_name() {
        let repository = "hello-world";
        let mut composition = Composition::with_repository(repository);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, repository, suffix);

        composition.configure_container_name(namespace, suffix);

        assert_eq!(
            composition.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }

    // Tests that the configurate_container_name method correctly sets the Composition's
    // container_name when the user has specified a container_name
    #[test]
    fn test_configurate_container_name_with_user_supplied_name() {
        let repository = "hello-world";
        let container_name = "this_is_a_container";
        let mut composition =
            Composition::with_repository(repository).with_container_name(container_name);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, container_name, suffix);

        composition.configure_container_name(namespace, suffix);

        assert_eq!(
            composition.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }

    // Tests that the configurate_container_name method replaces forward slashes with underscore
    // when a user provided name is given.
    // The docker daemon does not like forward slashes in container names.
    #[test]
    fn test_configurate_container_name_with_user_supplied_name_containing_slashes() {
        let repository = "hello-world";
        let container_name = "this/is/a_container";
        let expected_container_name = "this_is_a_container";

        let mut composition =
            Composition::with_repository(repository).with_container_name(container_name);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, expected_container_name, suffix);

        composition.configure_container_name(namespace, suffix);

        assert_eq!(
            composition.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }

    // Tests that the configurate_container_name method replaces forward slashes with underscore
    // when no user provided container name is provided.
    // The docker daemon does not like forward slashes in container names.
    #[test]
    fn test_configurate_container_name_without_user_supplied_name_containing_slashes() {
        let repository = "hello/world";
        let expected_container_name = "hello_world";

        let mut composition = Composition::with_repository(repository);

        let suffix = "test123";
        let namespace = "namespace";

        let expected_output = format!("{}-{}-{}", namespace, expected_container_name, suffix);

        composition.configure_container_name(namespace, suffix);

        assert_eq!(
            composition.container_name, expected_output,
            "container_name not configurated correctly"
        );
    }
}
