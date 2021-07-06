use dockertest::{Composition, DockerTest, Image, PullPolicy, Source, StaticManagementPolicy};
use lazy_static::lazy_static;
use std::sync::{Arc, Mutex};

lazy_static! {
    static ref STATIC_CONTAINER_ID: ContainerId = ContainerId::default();
}

#[test]
fn test_static_containers_runs() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let mut hello_world = Composition::with_image(img);
    hello_world.is_static_container(StaticManagementPolicy::DockerTest);

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
    hello_world.is_static_container(StaticManagementPolicy::DockerTest);

    test.add_composition(hello_world);

    test.run(|ops| async move {
        let handle = ops.handle("hello-world");
        let container_id = handle.id();
        assert!(STATIC_CONTAINER_ID.set_and_compare(container_id));
    });
}

#[test]
fn test_static_containers_references_the_same_container_within_test_binary_2() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let mut hello_world = Composition::with_image(img);
    hello_world.is_static_container(StaticManagementPolicy::DockerTest);

    test.add_composition(hello_world);

    test.run(|ops| async move {
        let handle = ops.handle("hello-world");
        let container_id = handle.id();
        assert!(STATIC_CONTAINER_ID.set_and_compare(container_id));
    });
}

#[derive(Debug)]
struct ContainerId {
    id: Arc<Mutex<Option<String>>>,
}

impl Default for ContainerId {
    fn default() -> ContainerId {
        ContainerId {
            id: Arc::new(Mutex::new(None)),
        }
    }
}

impl ContainerId {
    fn set_and_compare(&self, container_id: &str) -> bool {
        let mut id = self.id.lock().unwrap();
        if let Some(i) = &*id {
            i == container_id
        } else {
            *id = Some(container_id.to_string());
            true
        }
    }
}
