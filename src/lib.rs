//#![deny(missing_docs)]
#![deny(warnings)]
#![deny(rust_2018_idioms)]

pub mod container;
pub mod error;
pub mod image;

pub mod image_instance;

/// Module containing the public dockertest logic
pub mod test;

/// Module exporting the WaitFor trait
pub mod wait_for;

// Private module containing utility functions used for testing purposes
#[cfg(test)]
mod test_utils;

/// The intention of this crate is to facilitate setup and teardown
/// of containers in a test environment.
pub use crate::test::DockerTest;
