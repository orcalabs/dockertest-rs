//! Represent a concrete instance of an Image, before it is ran as a Container.

use crate::image::Image;
use crate::waitfor::{NoWait, WaitFor};

use std::collections::HashMap;
use tracing::{event, Level};

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
/// but before one can create a [crate::container:: OperationalContainer] from an image,
/// it must be augmented with information about how to start it, how to ensure it has been
/// started, environment variables and runtime commands.
/// Thus, this structure represents the concrete instance of an [Image] that will be started
/// and become a [crate::container::OperationalContainer].
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
    pub(crate) network_aliases: Option<Vec<String>>,

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
    pub(crate) wait: Box<dyn WaitFor>,

    /// The environmentable variables that will be passed to the container.
    pub(crate) env: HashMap<String, String>,

    /// The command to pass to the container.
    pub(crate) cmd: Vec<String>,

    /// The start policy of this container, codifing the inter-depdencies between containers.
    pub(crate) start_policy: StartPolicy,

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
    pub(crate) bind_mounts: Vec<String>,

    /// All user specified container name injections as environment variables.
    /// Tuple contains (handle, env).
    pub(crate) inject_container_name_env: Vec<(String, String)>,

    /// Port mapping (used for Windows-compatibility)
    pub(crate) port: Vec<(String, String)>,

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

    /// Tmpfs mount paths to create.
    ///
    /// These are destination paths within the container to create tmpfs filesystems for,
    /// they require no source paths.
    ///
    /// tmpfs details: <https://docs.docker.com/engine/storage/tmpfs/>
    pub(crate) tmpfs: Vec<String>,

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
            tmpfs: Vec::new(),
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
            tmpfs: Vec::new(),
        }
    }

    /// Adds the given tmpfs mount paths to this [Composition]
    ///
    /// See [tmpfs] for details.
    ///
    /// [tmpfs]: Composition::tmpfs
    #[cfg(target_os = "linux")]
    pub fn with_tmpfs(self, paths: Vec<String>) -> Composition {
        Composition {
            tmpfs: paths,
            ..self
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

    /// Assigns the full set of environmental variables available for the [OperationalContainer].
    ///
    /// Each key in the map should be the environmental variable name
    /// and its corresponding value will be set as its value.
    ///
    /// This method replaces the entire existing env map provided.
    ///
    /// [OperationalContainer]: crate::container::OperationalContainer
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
    /// Mapped host ports can be found via [crate::container::OperationalContainer::host_port] method.
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

    /// Appends the tmpfs mount path to the current set of tmpfs mount paths.
    ///
    /// NOTE: if [with_tmpfs] is called after a call to [tmpfs], all entries to the tmpfs vector
    /// added with [with_tmpfs] will be overwritten.
    ///
    /// Details:
    ///   - Only available on linux.
    ///   - Size of the tmpfs mount defaults to 50% of the hosts total RAM.
    ///   - Defaults to file mode '1777' (world-writable).
    ///
    /// [tmpfs]: Composition::tmpfs
    /// [with_tmpfs]: Composition::with_tmpfs
    #[cfg(target_os = "linux")]
    pub fn tmpfs<T: ToString>(&mut self, path: T) -> &mut Composition {
        self.tmpfs.push(path.to_string());
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
    pub(crate) fn is_static(&self) -> bool {
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
