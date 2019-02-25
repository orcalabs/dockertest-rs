use crate::container::Container;

/// Trait to wait for a container to be ready for service.
pub trait WaitFor {
    /// Should wait for the associated container to be ready for
    /// service.
    /// When the container is ready the method
    /// should return true.
    fn wait_for_ready(&self, _container: &Container) -> bool;
}

/// The default wait implementation for containers.
/// This implementation should wait for containers to appear
/// as running.
pub struct DefaultWait {}

impl WaitFor for DefaultWait {
    fn wait_for_ready(&self, _container: &Container) -> bool {
        false
    }
}
