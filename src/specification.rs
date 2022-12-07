//! The various mechanism available to specify a container to be part of the test.

use std::collections::HashMap;

use crate::{
    composition::{Composition, StaticManagementPolicy},
    waitfor::WaitFor,
    Image, LogOptions, StartPolicy,
};

mod private {
    pub use super::*;

    pub trait Sealed {}

    impl Sealed for TestBodySpecification {}
    impl Sealed for TestSuiteSpecification {}
    impl Sealed for DynamicSpecification {}
    impl Sealed for ExternalSpecification {}
}

/// Implemented by types that can represent and instruct how dockertest should interact with
/// a specific docker container.
pub trait ContainerSpecification: private::Sealed {
    /// Convert the specification of a container into a `Composition`.
    ///
    /// The `Composition` type is an internal implementation detail.
    fn into_composition(self) -> Composition;
}

macro_rules! impl_specify_container {
    ($s:ty) => {
        impl $s {
            /// Sets the [StartPolicy] when having to create and spin up this container.
            ///
            /// The start policy is needed to determine the order of containers to start in order
            /// to successfully start dependant containers.
            ///
            /// If not specified, [StartPolicy::Relaxed] is the default policy.
            pub fn set_start_policy(self, start_policy: StartPolicy) -> Self {
                Self {
                    composition: self.composition.with_start_policy(start_policy),
                }
            }

            /// Assign the full set of environment variables into the [RunningContainer].
            ///
            /// Each key in the map should be the environmental variable name
            /// and its corresponding value will be set as its value.
            ///
            /// This method replaces all existing environment variables previously provided.
            ///
            /// [RunningContainer]: crate::container::RunningContainer
            pub fn replace_env(self, env: HashMap<String, String>) -> Self {
                Self {
                    composition: self.composition.with_env(env),
                }
            }

            /// Modify a single environment variable available for the [RunningContainer].
            ///
            /// A [replace_env] call will undo what has been configured individually with this
            /// method.
            ///
            /// [RunningContainer]: crate::container::RunningContainer
            /// [replace_env]: Self::replace_env
            pub fn modify_env<T: ToString, S: ToString>(&mut self, name: T, value: S) -> &mut Self {
                self.composition.env(name, value);
                self
            }

            /// Assign the full set of command vector entries for the [RunningContainer].
            ///
            /// This method replaces all existing command vector entries previously provided.
            ///
            /// [RunningContainer]: crate::container::RunningContainer
            pub fn replace_cmd(self, cmd: Vec<String>) -> Self {
                Self {
                    composition: self.composition.with_cmd(cmd),
                }
            }

            /// Append a command entry into the command vector.
            ///
            /// A [replace_cmd] call will undo what has been configured individually with this
            /// method.
            ///
            /// [replace_cmd]: Self::replace_cmd
            pub fn append_cmd<T: ToString>(&mut self, cmd: T) -> &mut Self {
                self.composition.cmd(cmd);
                self
            }

            /// Allocate an ephemeral host port for all exposed ports specified in the container.
            ///
            /// Mapped host ports can be found via [RunningContainer::host_port] method.
            ///
            /// [RunningContainer::host_port]: crate::container::RunningContainer::host_port
            pub fn set_publish_all_ports(mut self, publish: bool) -> Self {
                self.composition.publish_all_ports(publish);
                self
            }

            /// Add a host port mapping to the container.
            ///
            /// This is useful when the host environment running docker cannot support IP routing
            /// within the docker network, such that test containers cannot communicate between
            /// themselves. This escape hatch allows the host to be involved to route traffic.
            ///
            /// This mechanism is not recommended, as concurrent tests utilizing the same host port
            /// will fail since the port is already in use. If utilizing the host is needed, it is
            /// recommended to use [set_publish_all_ports].
            ///
            /// This function can overwrite previously mapped ports, if invoked repeatedly.
            ///
            /// [set_publish_all_ports]: Self::set_publish_all_ports
            // TODO: Add a replace_port_map that takes (exported, host) tuples
            // TODO: Guarantee that a modification of one already exported/host value is
            // actually constructed and overriden correctly.
            pub fn modify_port_map(&mut self, exported: u32, host: u32) -> &mut Self {
                self.composition.port_map(exported, host);
                self
            }

            /// Specify the privilege mode of the started container.
            ///
            /// This may be required for some containers to run correctly.
            /// See the corresponding [docker reference] on this topic.
            ///
            /// [docker reference]: https://docs.docker.com/engine/reference/run/#runtime-privilege-and-linux-capabilities
            pub fn privileged(&mut self, privileged: bool) -> &mut Self {
                self.composition.privileged = privileged;
                self
            }

            /// Specify the privilege mode of the started container.
            ///
            /// This may be required for some containers to run correctly.
            /// See the corresponding [docker reference] on this topic.
            ///
            /// [docker reference]: https://docs.docker.com/engine/reference/run/#runtime-privilege-and-linux-capabilities
            pub fn set_privileged(mut self, privileged: bool) -> Self {
                self.composition.privileged = privileged;
                self
            }

            /// Specify a string handle used to retrieve a reference to the [RunningContainer]
            /// within the test body.
            ///
            /// This value defaults to the repository name of the image used when constructing
            /// this container specification.
            ///
            /// [RunningContainer]: crate::container::RunningContainer
            pub fn set_handle<T: ToString>(self, handle: T) -> Self {
                Self {
                    composition: self.composition.with_container_name(handle),
                }
            }

            /// Assign the full set of container name aliases on the docker network.
            pub fn replace_network_alias(self, aliases: Vec<String>) -> Self {
                Self {
                    composition: self.composition.with_alias(aliases),
                }
            }

            /// Add a single container name alias on the docker network.
            pub fn append_network_alias(&mut self, alias: String) -> &mut Self {
                self.composition.alias(alias);
                self
            }

            /// Set the [WaitFor] trait object for this container specification.
            ///
            /// If not specified, [RunningWait] will be the default value.
            ///
            /// [WaitFor]: crate::waitfor::WaitFor
            /// [RunningWait]: crate::waitfor::RunningWait
            pub fn set_wait_for(self, wait: Box<dyn WaitFor>) -> Self {
                Self {
                    composition: self.composition.with_wait_for(wait),
                }
            }

            /// Specify how to handle logging from the container.
            ///
            /// If not specified, [LogAction::Forward], [LogPolicy::OnError] and
            /// [LogSource::StdErr] is enabled. To clear the default options, invoke this method
            /// with a `None` value.
            ///
            /// [LogSource::StdErr]: crate::composition::LogSource
            /// [LogAction::Forward]: crate::composition::LogAction
            /// [LogPolicy::OnError]: crate::composition::LogPolicy
            pub fn set_log_options(self, log_options: Option<LogOptions>) -> Self {
                Self {
                    composition: self.composition.with_log_options(log_options),
                }
            }

            /// Add a named volume to this container.
            ///
            /// Named volumes can be shared between multiple containers. By specifying the same
            /// volume name across multiple container specifications, both container will be
            /// given access to the same volume.
            ///
            /// * `path_in_container` must be an absolute path.
            // TODO: Add a set_ variant
            pub fn modify_named_volume<T: ToString, S: ToString>(
                &mut self,
                volume_name: T,
                path_in_container: S,
            ) -> &mut Self {
                self.composition
                    .named_volume(volume_name, path_in_container);
                self
            }

            /// Add a bind mount to this container.
            ///
            /// A bind mount only exists for a single container, and maps a given file or directory
            /// from the host into the container.
            ///
            /// Use named volumes if you require shared data access between containers.
            ///
            /// * `host_path` can either point to a file or directory that must exist on the host.
            /// * `path_in_container` must be an absolute path.
            pub fn modify_bind_mount<T: ToString, S: ToString>(
                &mut self,
                host_path: T,
                path_in_container: S,
            ) -> &mut Self {
                self.composition.bind_mount(host_path, path_in_container);
                self
            }

            /// Inject the full, generated container name identified by `handle` into this
            /// container specification environment.
            ///
            /// This is used to establish inter communication between running containers
            /// controlled by dockertest. This is traditionally established through environment
            /// variables for connection details, and thus the DNS resolving capabilities within
            /// docker will map the container name into the correct IP address.
            ///
            /// To correctly use this feature, the `StartPolicy` between the dependant containers
            /// must be configured such that these connections can successfully be established.
            /// Dockertest will not make any attempt to verify the integrity of these dependencies.
            // TODO: naming
            // TODO: Refactor to use some reference mechanism
            pub fn inject_container_name<T: ToString, E: ToString>(
                &mut self,
                handle: T,
                env: E,
            ) -> &mut Self {
                self.composition.inject_container_name(handle, env);
                self
            }
        }
    };
}

/// A specification of a container external to dockertest.
///
/// The management and lifecycle of this container is unknown and not touched by dockertest.
#[derive(Clone, Debug)]
pub struct ExternalSpecification {
    name: String,
}

impl ExternalSpecification {
    /// Create a new [ExternalSpecification] with the full container name of an existing container.
    pub fn with_container_name<T: ToString>(name: T) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

impl ContainerSpecification for ExternalSpecification {
    fn into_composition(self) -> Composition {
        let mut c = Composition::with_repository("NOT REQUIRED").with_container_name(self.name);
        c.static_container(StaticManagementPolicy::External);

        c
    }
}

/// A specification of a container that shall live across all tests within the testsuite,
/// and should be appropriately shut down once all tests in the suite have terminated.
///
/// NOTE: [TestSuiteSpecification] is an experimental management mode.
#[derive(Clone, Debug)]
pub struct TestSuiteSpecification {
    composition: Composition,
}

impl ContainerSpecification for TestSuiteSpecification {
    fn into_composition(mut self) -> Composition {
        self.composition
            .static_container(StaticManagementPolicy::Internal);

        self.composition
    }
}

impl TestSuiteSpecification {
    /// Create a new [TestSuiteSpecification] based on the image pointed to by this repository.
    ///
    /// This will internally create an [Image] based on the provided repository name,
    /// and default the tag to `latest`.
    ///
    /// NOTE: [TestSuiteSpecification] is an experimental management mode.
    pub fn with_repository<T: ToString>(repository: T) -> Self {
        Self {
            composition: Composition::with_repository(repository),
        }
    }

    /// Create a new [TestSuiteSpecification] based on provided [Image].
    ///
    /// NOTE: [TestSuiteSpecification] is an experimental management mode.
    pub fn with_image(image: Image) -> Self {
        Self {
            composition: Composition::with_image(image),
        }
    }
}

impl_specify_container!(TestSuiteSpecification);

/// The standard container specification.
///
/// This containers' lifecycle is managed entirely within a single dockertest test body run.
/// It is created, started, and ensured exited all within the scope of the test body.
pub struct TestBodySpecification {
    composition: Composition,
}

impl ContainerSpecification for TestBodySpecification {
    fn into_composition(self) -> Composition {
        self.composition
    }
}

impl TestBodySpecification {
    /// Create a new [TestBodySpecification] based on the image pointed to by this repository.
    ///
    /// This will internally create an [Image] based on the provided repository name,
    /// and default the tag to `latest`.
    pub fn with_repository<T: ToString>(repository: T) -> Self {
        Self {
            composition: Composition::with_repository(repository),
        }
    }

    /// Create a new [TestBodySpecification] based on provided [Image].
    pub fn with_image(image: Image) -> Self {
        Self {
            composition: Composition::with_image(image),
        }
    }
}

impl_specify_container!(TestBodySpecification);

/// A full specification of a container whose lifecycle is partially managed by dockertest.
///
/// A dynamic container specification has the ability to create and re-use an existing
/// container matching its properties, but will never attempt to terminate the container.
#[derive(Clone, Debug)]
pub struct DynamicSpecification {
    composition: Composition,
}

impl ContainerSpecification for DynamicSpecification {
    fn into_composition(mut self) -> Composition {
        self.composition
            .static_container(StaticManagementPolicy::Dynamic);

        self.composition
    }
}

impl DynamicSpecification {
    /// Create a new [TestSuiteSpecification] based on the image pointed to by this repository.
    ///
    /// The provided `container_name` will be used to locate an existing container, or create
    /// the container if it does not exist.
    pub fn with_repository<T: ToString, S: ToString>(repository: T, container_name: S) -> Self {
        Self {
            composition: Composition::with_repository(repository)
                .with_container_name(container_name),
        }
    }

    /// Create a new [TestBodySpecification] based on provided [Image].
    ///
    /// The provided `container_name` will be used to locate an existing container, or create
    /// the container if it does not exist.
    pub fn with_image<S: ToString>(image: Image, container_name: S) -> Self {
        Self {
            composition: Composition::with_image(image).with_container_name(container_name),
        }
    }
}

impl_specify_container!(DynamicSpecification);
