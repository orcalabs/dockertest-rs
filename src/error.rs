//! Errors that can arise from dockertest.

use thiserror::Error;

/// Public library error conditions.
#[derive(Error, Debug)]
#[allow(missing_docs)]
pub enum DockerTestError {
    #[error("docker daemon interaction error")]
    Daemon(String),
    #[error("recoverable error condition")]
    Recoverable(String),
    #[error("container teardown error")]
    Teardown(String),
    #[error("pulling image from remote repository failed")]
    Pull(String),
    #[error("startup condition not fulfilled `{0}`")]
    Startup(String),
    #[error("processing error condition")]
    Processing(String),
    #[error("test body failure `{0}`")]
    TestBody(String),

    /// Catch-all IO error condition
    #[error("IO error condition")]
    Io {
        #[from]
        source: std::io::Error,
    },
}
