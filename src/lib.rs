#![deny(missing_docs)]
#![deny(warnings)]
#![deny(rust_2018_idioms)]

//! dockertest-rs is a testing & automation abstraction for Docker.
//!
//! The primary utility for this crate is to easily employ docker in test infrastructure,
//! with the following key features:
//! * Ensure that the docker container is running prior to test code.
//!  * Support multiple containers per test.
//!  * Support multiple containers from same image, with different configurations.
//! * Retrieve [Image] from remote source according to [PullPolicy].
//! * Support multiple [Remote] registries, which can be individually assigned to [Image].
//! * Dictate how each [Container] is created and operated from an [Image]
//!   through an [ImageInstance].
//!  * This allows us to have muliple containers with the same Image,
//!    but with different start conditions.
//! * Control each [ImageInstance] condition for when it is deemed running through [WaitFor].
//!  * There exists multiple convenient [WaitFor] implementations, however, user-supplied
//!    implementations through the trait can be provided.
//! * Control the [StartPolicy] of each [ImageInstance]. For inter-dependant
//!   containers, a Strict policy can be sat, and they will be started in succession until
//!   their [WaitFor] condition is met, according to the order they where added to [DockerTest].
//!
//! Once the [DockerTest] test is `run`, the provided closure will be ran once
//! all predicates for each supplied [ImageInstance] has successfully been fulfilled.
//! The closure is provided with one [DockerOperations] parameter, allowing the test body
//! to interact with [DockerTest] and each individual [Container].
//! The reference to each [Container] is queried through a handle, which is the user provided
//! container name, specified in [ImageInstance] through [with_container_name].
//!
//! Once the reference to a [Container] is retrieved, one may call all methods public methods
//! to interact with it. This includes retrieving the [host_port] that a port in the [Container]
//! is mapped to on the host.
//!
//!
//! # Example
//!
//! ```rust,ignore
//! let source = Source::DockerHub(PullPolicy::IfNotPresent);
//! let mut test = DockerTest::new().with_default_source(source);
//!
//! let repo = "postgres";
//! let postgres = ImageInstance::with_repository(repo);
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
//! [ImageInstance]: image_instance/struct.ImageInstance.html
//! [with_container_name]: image_instance/struct.ImageInstance.html#method.with_container_name
//! [PullPolicy]: image/enum.PullPolicy.html
//! [Remote]: image/struct.Remote.html
//! [StartPolicy]: image_instance/enum.StartPolicy.html
//! [WaitFor]: wait_for/trait.WaitFor.html
//! [DockerTest]: test/struct.DockerTest.html

pub mod container;
pub mod dockertest;
pub mod error;
pub mod image;
pub mod image_instance;
pub mod wait_for;

// Private module containing utility functions used for testing purposes
#[cfg(test)]
mod test_utils;

pub use crate::dockertest::DockerTest;
