use crate::container::Container;
use failure::Error;
use futures::future::{self, Future};

/// Trait to wait for a container to be ready for service.
pub trait WaitFor {
    /// Should wait for the associated container to be ready for
    /// service.
    /// When the container is ready the method
    /// should return true.
    fn wait_for_ready(
        &self,
        container: Container,
    ) -> Box<dyn Future<Item = Container, Error = Error>>;
}

/// The default wait implementation for containers.
/// This implementation should wait for containers to appear
/// as running.
pub struct DefaultWait {}

impl WaitFor for DefaultWait {
    fn wait_for_ready(
        &self,
        container: Container,
    ) -> Box<dyn Future<Item = Container, Error = Error>> {
        Box::new(future::ok(container))
    }
}

#[cfg(test)]
mod tests {
    use crate::container::Container;
    use crate::wait_for::{DefaultWait, WaitFor};
    use shiplift;
    use std::rc::Rc;
    use tokio::runtime::current_thread;

    // Tests that WaitFor implementation for
    // DefaultWait returns ok
    #[test]
    fn test_default_returns_ok() {
        let mut rt = current_thread::Runtime::new().expect("failed to start tokio runtime");

        let wait = DefaultWait {};

        let container_name = "this_is_a_name".to_string();
        let id = "this_is_an_id".to_string();
        let handle_key = "this_is_a_handle_key";

        let container = Container::new(
            &container_name,
            &id,
            handle_key,
            Rc::new(shiplift::Docker::new()),
        );

        let res = rt.block_on(wait.wait_for_ready(container));
        assert!(res.is_ok(), "should always return ok with DefaultWait");

        let container = res.expect("failed to get container");

        assert_eq!(
            container_name,
            container.name(),
            "returned container is not identical"
        );
    }
}
