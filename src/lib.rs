#![deny(missing_docs)]
#![deny(warnings)]
#![deny(rust_2018_idioms)]

//! _dockertest_ is a testing and automation abstraction for Docker.
//!
//! The primary utility for this crate is to easily employ docker in test infrastructure,
//! with the following key features:
//! * Ensure that the docker container is running prior to test code.
//!  * Support multiple containers per test.
//!  * Support multiple containers from same image, with different configurations.
//! * Retrieve [Image] from remote source according to [PullPolicy].
//! * Support multiple [Remote] registries, which can be individually assigned to [Image].
//! * Dictate how each [Container] is created and operated from an [Image]
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
//! to interact with [DockerTest] and each individual [Container].
//! The reference to each [Container] is queried through a handle, which is the user provided
//! container name, specified in [Composition] through [with_container_name].
//!
//! Once the reference to a [Container] is retrieved, one may call all methods public methods
//! to interact with it. This includes retrieving the [host_port] that a port in the [Container]
//! is mapped to on the host.
//!
//! # Handle - referencing the same container throughout your test
//!
//! _dockertest_ assigns a `handle` to each [Composition], which carries over to a
//! [RunningContainer]. When writing a test, one will reference the intended object through its
//! handle.
//!
//! By default, the handle is auto-assigned to be the repository name of the [Composition].
//!
//! The user may change the `handle` by changing the container name (as seen from the user
//! - the final container name will be disambiguated for each _dockertest_) through the
//! [with_container_name] builder method on [Composition].
//!
//! If the test includes multiple [Composition]s with the same handle,
//! attempting to reference one that has multiple occurrences will fail the test (e.g., panic).
//!
//! # WaitFor - Control how to determine when the container is ready
//!
//! Each [Composition] require a trait object of [WaitFor] whose method [wait_for_ready]
//! must resolve until the container can become a [RunningContainer].
//! This trait may be implemented and supplied to [Composition] through [with_wait_for].
//!
//! # Example
//!
//! ```rust,ignore
//! let source = Source::DockerHub(PullPolicy::IfNotPresent);
//! let mut test = DockerTest::new().with_default_source(source);
//!
//! let repo = "postgres";
//! let postgres = Composition::with_repository(repo);
//!
//! test.add_instance(postgres);
//!
//! test.run(|ops| {
//!     let container = ops.handle("postgres").expect("retrieve postgres container");
//!     let host_port = container.host_port(5432);
//!     let conn_string = format!("postgres://postgres:postgres@localhost:{}", host_port);
//!     let pgconn = PgConnection::establish(&conn_string);
//!
//!     assert!(
//!         pgconn.is_ok(),
//!         "failed to establish connection to postgres docker"
//!     );
//! });
//! ```
//!
//! [Container]: container/struct.Container.html
//! [host_port]: container/struct.Container.html#method.host_port
//! [DockerOperations]: test/struct.DockerOperations.html
//! [Image]: image/struct.Image.html
//! [Composition]: composition/struct.Composition.html
//! [with_container_name]: composition/struct.Composition.html#method.with_container_name
//! [PendingContainer]: container/struct.Container.html
//! [PullPolicy]: image/enum.PullPolicy.html
//! [Remote]: image/struct.Remote.html
//! [RunningContainer]: container/struct.Container.html
//! [StartPolicy]: composition/enum.StartPolicy.html
//! [WaitFor]: waitfor/trait.WaitFor.html
//! [wait_for_ready]: waitfor/trait.WaitFor.html#method.wait_for_ready
//! [with_wait_for]: waitfor/struct.Composition.html#method.with_wait_for
//! [DockerTest]: test/struct.DockerTest.html

mod composition;
mod container;
mod dockertest;
pub mod error;
mod image;
pub mod waitfor;

// Private module containing utility functions used for testing purposes
#[cfg(test)]
mod test_utils;

pub use crate::composition::{Composition, StartPolicy};
pub use crate::container::Container;
pub use crate::dockertest::{DockerOperations, DockerTest};
pub use crate::image::{Image, PullPolicy, Remote, Source};
