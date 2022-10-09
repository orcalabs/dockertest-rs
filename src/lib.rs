#![deny(missing_docs)]
#![deny(warnings)]
#![deny(rust_2018_idioms)]
#![deny(rustdoc::broken_intra_doc_links)]

//! _dockertest_ is a testing and automation abstraction for Docker.
//!
//! This testing library enables control and lifecycle management with docker containers
//! from an integration test, taking full care of start and cleanup phases. Containers can
//! be backed by various container registries, even behind authentication barrier.
//!
//! The main entrypoint to construct and a run a dockertest powered integration test starts
//! with [DockerTest].
//!
//! The underlying docker engine to use can be configured through:
//! - UNIX domain socket (linux)
//! - Named pipes (windows)
//! - TCP with TLS
//! - Piped through a docker-in-docker container where the execution occurs, to run on the
//! underlying docker engine.
//!
//! The main bread-and-butter of this library is the ability to specify which containers are
//! required for a test, and how one should ensure that the container is properly running prior
//! to starting the actual test body. Furthermore, dockertest supports interacting with existing
//! containers, without interfering with its lifecycle management.
//!
//! ## Lifecycle management
//!
//! There are multiple ways to control and interact with containers, with various trade-offs.
//!
//! ### Full isolation - `TestBodySpecification`
//!
//! The container is created, started, and cleaned up entirely within a single test body. This is
//! the highest level of isolation, and no other tests can interfere with this container. The main
//! drawback of this method is the high wall-time needed to start these containers. If there are
//! many integration tests in this mode, the total test run-time will be made primarily by the
//! container management overhead.
//!
//! ### Externally managed - `ExternalSpecification`
//!
//! The container is _never_ touched by dockertest. It is assumed to be started by some external
//! entity, and the container is never teared down by dockertest. Prior to running the test body,
//! the existence of the external container is verified.
//!
//! ### Conditionally managed - `DynamicSpecification`
//!
//! The containers lifecycle is only managed for creation, but not deletion. When multiple
//! specifications relate to the same underlying container, dockertest will attempt to locate
//! an existing container, and reuse this container to fulfill the specification. Once the
//! test body exists, the container will be left running.
//!
//! This mode is useful in when there are multiple tests, over a long period of development time,
//! that can utilize the same underlying container without causing cross-test contamination.
//! This will lead to significantly faster test execution time.
//!
//! # `WaitFor` - determining when a container is ready
//!
//! Each container that dockertest creates and starts must also have a policy to detect
//! when the container it has started is actually ready to start serving the test body.
//! This is achived through a trait object [WaitFor], where an implementation can be provided
//! outside of dockertest.
//!
//! The batteries included implementations are:
//! * [RunningWait] - wait for the container to report _running_ status.
//! * [ExitedWait] - wait for the container to report _exited_ status.
//! * [NoWait] - don't wait for anything
//! * [MessageWait] - wait for the following message to appear in the log stream.
//!
//! # Environment variables
//!
//! The following set of environment variables can impact running tests utilizing dockertest.
//!
//! ## Prune policy
//!
//! By default, dockertest will stop and remove all containers and created volumes
//! regardless of execution result. You can control this policy by setting the environment variable
//! `DOCKERTEST_PRUNE`:
//! * `always`: default remove everything
//! * `never`: leave all containers running
//! * `stop_on_failure`: stop containers on execution failure
//! * `running_on_failure`: leave containers running on execution failure
//!
//! ## Dockertest in Docker
//!
//! If the execution environment of running dockertest is itself a docker-in-docker container, one
//! may have connectivity issues between the test body code, and the container dependencies.
//! To ensure connectivity, dockertest must be instructed about the name/identifier of the
//! execution container to include it into the docker network of the test containers.
//! This is usually because the docker-in-docker docker daemon connection is routed to
//! the underlying host itself.
//!
//! `DOCKERTEST_CONTAINER_ID_INJECT_TO_NETWORK=your_container_id/name`
//!
//! # Example
//!
//! ```rust
//!
//! use dockertest::{TestBodySpecification, DockerTest};
//! use std::sync::{Arc, Mutex};
//!
//! #[test]
//! fn hello_world_test() {
//!     // Define our test instance
//!     let mut test = DockerTest::new();
//!
//!     // A container specification can have multiple properties, depending on how the
//!     // lifecycle management of the container should be handled by dockertest.
//!     //
//!     // For any container specification where dockertest needs to create and start the container,
//!     // we must provide enough information to construct a composition of
//!     // an Image configured with provided environment variables, arguments, StartPolicy, etc.
//!     let hello = TestBodySpecification::with_repository("hello-world");
//!
//!     // Populate the test instance.
//!     // The order of compositions added reflect the execution order (depending on StartPolicy).
//!     test.provide_container(hello);
//!
//!     let has_ran: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
//!     let has_ran_test = has_ran.clone();
//!     test.run(|ops| async move {
//!         // A handle to operate on the Container.
//!         let _container = ops.handle("hello-world");
//!
//!         // The container is in a running state at this point.
//!         // Depending on the Image, it may exit on its own (like this hello-world image)
//!         let mut ran = has_ran_test.lock().unwrap();
//!         *ran = true;
//!     });
//!
//!     let ran = has_ran.lock().unwrap();
//!     assert!(*ran);
//! }
//! ```
//!
//! [WaitFor]: crate::waitfor::WaitFor
//! [RunningWait]: crate::waitfor::RunningWait
//! [ExitedWait]: crate::waitfor::ExitedWait
//! [NoWait]: crate::waitfor::NoWait
//! [MessageWait]: crate::waitfor::MessageWait

mod composition;
mod container;
mod dockertest;
mod engine;
mod error;
mod image;
mod runner;
mod specification;
mod static_container;
// We only make this public because a function is used in our integration test
#[doc(hidden)]
pub mod utils;
pub mod waitfor;

// Private module containing utility functions used for testing purposes
#[cfg(test)]
mod test_utils;

pub use crate::composition::{LogAction, LogOptions, LogPolicy, LogSource, StartPolicy};
pub use crate::container::{PendingContainer, RunningContainer};
pub use crate::dockertest::DockerTest;
pub use crate::dockertest::Network;
pub use crate::error::DockerTestError;
pub use crate::image::{Image, PullPolicy, RegistryCredentials, Source};
pub use crate::runner::DockerOperations;
pub use crate::specification::{
    ContainerSpecification, DynamicSpecification, ExternalSpecification, TestBodySpecification,
    TestSuiteSpecification,
};
