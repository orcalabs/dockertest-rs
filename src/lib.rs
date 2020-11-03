#![deny(missing_docs)]
#![deny(warnings)]
#![deny(rust_2018_idioms)]
#![deny(broken_intra_doc_links)]

//! _dockertest_ is a testing and automation abstraction for Docker.
//!
//! The primary utility for this crate is to easily employ docker in test infrastructure,
//! with the following key features:
//! * Ensure that the docker container is running prior to test code.
//!  * Support multiple containers per test.
//!  * Support multiple containers from same image, with different configurations.
//! * Retrieve [Image] from remote source according to [PullPolicy].
//! * Support multiple [Remote] registries, which can be individually assigned to [Image].
//! * Dictate how each [RunningContainer] is created and operated from an [Image]
//!   through a [Composition].
//!  * This allows us to have muliple containers with the same Image,
//!    but with different start conditions.
//! * Control each [Composition] condition for when it is deemed running through [WaitFor].
//!  * There exists multiple convenient [WaitFor] implementations, however, user-supplied
//!    implementations through the trait can be provided.
//! * Control the [StartPolicy] of each [Composition]. For inter-dependant
//!   containers, a Strict policy can be sat, and they will be started in succession until
//!   their [WaitFor] condition is met, according to the order they where added to [DockerTest].
//!
//! Once the [DockerTest] test is `run`, the provided closure will be ran once
//! all predicates for each supplied [Composition] has successfully been fulfilled.
//! The closure is provided with one [DockerOperations] parameter, allowing the test body
//! to interact with [DockerTest] and each individual [RunningContainer].
//! The reference to each [RunningContainer] is queried through a handle, which is the user-provided
//! container name, specified in [Composition] through [with_container_name]. See the Handle
//! section.
//!
//! # Handle - referencing the same container throughout your test
//!
//! _dockertest_ assigns a `handle` to each [Composition], which carries over to a
//! [RunningContainer]. When writing a test, one will reference the intended object through its
//! handle.
//!
//! By default, the handle is auto-assigned to be the repository name of the [Composition].
//!
//! The user may change the handle by changing the container name (as seen from the user
//! - the final container name will be disambiguated for each _dockertest_) through the
//! [with_container_name] builder method on [Composition].
//!
//! If the test includes multiple [Composition]s with the same handle,
//! attempting to reference one that has multiple occurrences will fail the test at runtime.
//!
//! # WaitFor - Control how to determine when the container is ready
//!
//! Each [Composition] require a trait object of [WaitFor] whose method [wait_for_ready]
//! must resolve until the container can become a [RunningContainer].
//! This trait may be implemented and supplied to [Composition] through [with_wait_for].
//!
//! The batteries included implementations are:
//! * [RunningWait] - wait for the container to report _running_ status.
//! * [ExitedWait] - wait for the container to report _exited_ status.
//! * [NoWait] - don't wait for anything
//! * [MessageWait] - wait for the following message to appear in the log stream.
//!
//! # Prune policy
//!
//! By default, _dockertest_ will stop and remove all containers and created volumes
//! regardless of execution result. You can control this policy by setting the environment variable
//! `DOCKERTEST_PRUNE`:
//! * "always": [default] remove everything
//! * "never": leave all containers running
//! * "stop_on_failure": stop containers on execution failure
//! * "running_on_failure": leave containers running on execution failure
//!
//! # Viewing logs of test execution
//! _dockertest_ utilizes the `tracing` log infrastructure. To enable this log output,
//! you must perform enable a subscriber to handle the events.
//! This can easily be done by for instance the wrapper crate `test-env-log`, that provides
//! a new impl of the `#[test]` attribute.
//!
//! .Cargo.toml
//! ```no_compile
//! [dev-dependencies]
//! tracing = "0.1.13"
//! tracing-subscriber = "0.2"
//! test-env-log = { version = "0.2", default-features = false, features = ["trace"] }
//! ```
//!
//! .Top of test file
//! ```
//! use test_env_log::test;
//! ```
//!
//! # Example
//!
//! ```rust
//! use diesel::pg::PgConnection;
//! use diesel::prelude::*;
//! use dockertest::waitfor::{MessageSource, MessageWait};
//! use dockertest::{Composition, DockerTest, PullPolicy, Source};
//! use std::rc::Rc;
//!
//! // Define our test
//! let source = Source::DockerHub(PullPolicy::IfNotPresent);
//! let mut test = DockerTest::new().with_default_source(source);
//!
//! // Define our Composition - the Image we will start and end up as our RunningContainer
//! let mut postgres = Composition::with_repository("postgres").with_wait_for(Box::new(MessageWait {
//!     message: "database system is ready to accept connections".to_string(),
//!     source: MessageSource::Stderr,
//!     timeout: 20,
//! }));
//! postgres.env("POSTGRES_PASSWORD", "password");
//! test.add_composition(postgres);
//!
//! // Run the test body
//! test.run(|ops| async move {
//!     let container = ops.handle("postgres");
//!     let conn_string = format!("postgres://postgres:password@{}:{}", container.ip(), 5432);
//!     let pgconn = PgConnection::establish(&conn_string);
//!
//!     // Perform your database operations here
//!     assert!(
//!         pgconn.is_ok(),
//!         "failed to establish connection to postgres docker"
//!     );
//! });
//! ```
//!
//! [Composition]: struct.Composition.html
//! [DockerOperations]: struct.DockerOperations.html
//! [DockerTest]: struct.DockerTest.html
//! [ExitedWait]: waitfor/struct.ExitedWait.html
//! [host_port]: struct.Container.html#method.host_port
//! [Image]: struct.Image.html
//! [MessageWait]: waitfor/struct.MessageWait.html
//! [NoWait]: waitfor/struct.NoWait.html
//! [PendingContainer]: struct.Container.html
//! [PullPolicy]: enum.PullPolicy.html
//! [Remote]: struct.Remote.html
//! [RunningContainer]: struct.RunningContainer.html
//! [RunningWait]: waitfor/struct.RunningWait.html
//! [StartPolicy]: enum.StartPolicy.html
//! [WaitFor]: waitfor/trait.WaitFor.html
//! [wait_for_ready]: waitfor/trait.WaitFor.html#method.wait_for_ready
//! [with_container_name]: struct.Composition.html#method.with_container_name
//! [with_wait_for]: struct.Composition.html#method.with_wait_for

mod composition;
mod container;
mod dockertest;
mod error;
mod image;
pub mod waitfor;

// Private module containing utility functions used for testing purposes
#[cfg(test)]
mod test_utils;

pub use crate::composition::{Composition, StartPolicy};
pub use crate::container::{PendingContainer, RunningContainer};
pub use crate::dockertest::{DockerOperations, DockerTest};
pub use crate::error::DockerTestError;
pub use crate::image::{Image, PullPolicy, Remote, Source};
