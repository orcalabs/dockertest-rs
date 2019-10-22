//! Custom error types library.

use failure::{Backtrace, Context, Fail};
use std::fmt;

/// The error type of DockerTest.
#[derive(Debug)]
pub struct DockerError {
    ctx: Context<DockerErrorKind>,
}

impl DockerError {
    /// Retrieve the underlying error enum.
    pub fn kind(&self) -> &DockerErrorKind {
        self.ctx.get_context()
    }

    pub(crate) fn recoverable<T: AsRef<str>>(s: T) -> DockerError {
        DockerError::from(DockerErrorKind::Recoverable(s.as_ref().to_string()))
    }

    pub(crate) fn daemon<T: AsRef<str>>(s: T) -> DockerError {
        DockerError::from(DockerErrorKind::Daemon(s.as_ref().to_string()))
    }

    pub(crate) fn teardown<T: AsRef<str>>(s: T) -> DockerError {
        DockerError::from(DockerErrorKind::Teardown(s.as_ref().to_string()))
    }

    pub(crate) fn pull<T: AsRef<str>>(s: T) -> DockerError {
        DockerError::from(DockerErrorKind::Pull(s.as_ref().to_string()))
    }

    pub(crate) fn startup<T: AsRef<str>>(s: T) -> DockerError {
        DockerError::from(DockerErrorKind::Startup(s.as_ref().to_string()))
    }
}

impl fmt::Display for DockerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.ctx.fmt(f)
    }
}

impl Fail for DockerError {
    fn cause(&self) -> Option<&dyn Fail> {
        self.ctx.cause()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.ctx.backtrace()
    }
}

/// The various error conditions that can occur from dockertest.
pub enum DockerErrorKind {
    /// Daemon interaction error.
    Daemon(String),
    /// Error condition is recoverable.
    Recoverable(String),
    /// Error upon teardown of container.
    Teardown(String),
    /// Pulling image from remote repository failed.
    Pull(String),
    /// Startup condition not fulfilled.
    Startup(String),
}

impl fmt::Display for DockerErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DockerErrorKind::Daemon(msg) => write!(f, "[docker daemon] '{}'", msg),
            DockerErrorKind::Recoverable(msg) => write!(f, "[recoverable] '{}'", msg),
            DockerErrorKind::Teardown(msg) => write!(f, "[teardown] '{}'", msg),

            DockerErrorKind::Pull(msg) => write!(f, "[pull] '{}'", msg),

            DockerErrorKind::Startup(msg) => write!(f, "[startup] '{}'", msg),
        }
    }
}

impl From<DockerErrorKind> for DockerError {
    fn from(kind: DockerErrorKind) -> DockerError {
        DockerError::from(Context::new(kind))
    }
}

impl From<Context<DockerErrorKind>> for DockerError {
    fn from(ctx: Context<DockerErrorKind>) -> DockerError {
        DockerError { ctx }
    }
}
