use crate::error::DockerError;
use futures::future::Future;
use shiplift;
use shiplift::builder::RmContainerOptions;
use std::rc::Rc;

// TODO: do host_port mapping
/// Represents a running container
#[derive(Clone)]
pub struct Container {
    /// The docker client
    client: Rc<shiplift::Docker>,

    /// Name of the container, defaults to the
    /// repository name of the image.
    name: String,

    /// Id of the running container.
    id: String,
}

impl Container {
    /// Creates a new Container object with the given
    /// values.
    pub(crate) fn new<T: ToString>(name: &T, id: &T, client: Rc<shiplift::Docker>) -> Container {
        Container {
            client,
            name: name.to_string(),
            id: id.to_string(),
        }
    }

    /// Returns which host port the given container port is mapped to.
    pub fn host_port(&self, _container_port: u32) -> u32 {
        0
    }

    /// Returns the name of container
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the id of container
    #[cfg(test)]
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Forcefully removes the container and consumes self.
    pub(crate) fn remove(self) -> impl Future<Item = (), Error = DockerError> {
        let ops = RmContainerOptions::builder().force(true).build();
        shiplift::Container::new(&self.client, self.id)
            .remove(ops)
            .map_err(|e| DockerError::daemon(format!("failed to remove container: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use crate::container::Container;
    use shiplift;
    use std::rc::Rc;

    // Tests that we can create a new container with the new method, and
    // that the correct struct members are set.
    #[test]
    fn test_new_container() {
        let client = Rc::new(shiplift::Docker::new());

        let id = "this_is_an_id".to_string();
        let name = "this_is_a_container_name".to_string();

        let container = Container::new(&name, &id, client);
        assert_eq!(id, container.id, "wrong id set in container creation");
        assert_eq!(name, container.name, "wrong name set in container creation");
        assert_eq!(
            name,
            container.name(),
            "container name getter returns wrong value"
        );
    }
}
