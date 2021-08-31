use dockertest::{Composition, DockerTest, Image, PullPolicy, Source, StaticManagementPolicy};
use lazy_static::lazy_static;
use std::sync::{Arc, Mutex};

lazy_static! {
    // As the container might re-created due to one of the tests completing before the next one
    // starts we cannot rely on container id as they will differ after creating the container
    // again.
    static ref STATIC_CONTAINER_NAME: ContainerName = ContainerName::default();
}

#[test]
fn test_static_containers_runs() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let mut hello_world = Composition::with_image(img);
    hello_world.static_container(StaticManagementPolicy::DockerTest);

    test.add_composition(hello_world);

    test.run(|_ops| async {
        assert!(true);
    });
}

#[test]
fn test_static_containers_references_the_same_container_within_test_binary() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let mut hello_world = Composition::with_image(img);
    hello_world.static_container(StaticManagementPolicy::DockerTest);

    test.add_composition(hello_world);

    test.run(|ops| async move {
        let handle = ops.handle("hello-world");
        let container_name = handle.name();
        assert!(STATIC_CONTAINER_NAME.set_and_compare(container_name));
    });
}

#[test]
fn test_static_containers_references_the_same_container_within_test_binary_2() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let mut hello_world = Composition::with_image(img);
    hello_world.static_container(StaticManagementPolicy::DockerTest);

    test.add_composition(hello_world);

    test.run(|ops| async move {
        let handle = ops.handle("hello-world");
        let container_name = handle.name();
        assert!(STATIC_CONTAINER_NAME.set_and_compare(container_name));
    });
}

#[derive(Debug)]
struct ContainerName {
    name: Arc<Mutex<Option<String>>>,
}

impl Default for ContainerName {
    fn default() -> ContainerName {
        ContainerName {
            name: Arc::new(Mutex::new(None)),
        }
    }
}

impl ContainerName {
    fn set_and_compare(&self, container_name: &str) -> bool {
        let mut name = self.name.lock().unwrap();
        if let Some(i) = &*name {
            i == container_name
        } else {
            *name = Some(container_name.to_string());
            true
        }
    }
}
