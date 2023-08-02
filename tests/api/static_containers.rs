//! Integration test for static dockertest containers,

use dockertest::{
    utils::{connect_with_local_or_tls_defaults, generate_random_string},
    DockerTest, DynamicSpecification, ExternalSpecification, Network, Source,
    TestBodySpecification, TestSuiteSpecification,
};

use bollard::container::{Config, CreateContainerOptions, StartContainerOptions};
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
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let hello_world = TestSuiteSpecification::with_repository(&repo);
    test.provide_container(hello_world);

    test.run(|_ops| async {
        assert!(true);
    });
}

#[tokio::test]
async fn test_external_static_container_handle_resolves_correctly_mixed_with_others() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);
    let repo = "hello-world";

    let d1_name = "frantic";
    let d2_name = "marvelous";
    let s1_name = format!("extravagant-{}", generate_random_string(20));

    let d1 = TestBodySpecification::with_repository(repo).set_handle(d1_name);
    let d2 = TestBodySpecification::with_repository(repo).set_handle(d2_name);
    let s1 = ExternalSpecification::with_container_name(&s1_name);

    test.provide_container(d1)
        .provide_container(s1)
        .provide_container(d2);

    // Run the external container
    let client = connect_with_local_or_tls_defaults().expect("connect to docker engine");
    let config = Config::<String> {
        image: Some(
            client
                .inspect_image(&format!("{}:{}", "dockertest-rs/hello", "latest"))
                .await
                .map(|res| res.id.unwrap())
                .expect("should get image id"),
        ),
        ..Default::default()
    };
    let options = Some(CreateContainerOptions {
        name: &s1_name,
        platform: None,
    });
    let id = client
        .create_container(options, config)
        .await
        .expect("create external container")
        .id;
    client
        .start_container(&id, None::<StartContainerOptions<String>>)
        .await
        .expect("start external container");

    test.run_async(|ops| async move {
        assert!(ops.handle(&s1_name).name().contains(&s1_name));
        assert!(ops.handle(d1_name).name().contains(d1_name));
        assert!(ops.handle(d2_name).name().contains(d2_name));
    })
    .await;
}

#[test]
fn test_static_containers_references_the_same_container_within_test_binary() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let hello_world = TestSuiteSpecification::with_repository(&repo);
    test.provide_container(hello_world);

    test.run(|ops| async move {
        let handle = ops.handle("hello-world");
        let container_name = handle.name();
        assert!(STATIC_CONTAINER_NAME.set_and_compare(container_name));
    });
}

#[test]
fn test_static_containers_references_the_same_container_within_test_binary_2() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let hello_world = TestSuiteSpecification::with_repository(&repo);
    test.provide_container(hello_world);

    test.run(|ops| async move {
        let handle = ops.handle(&repo);
        let container_name = handle.name();
        assert!(STATIC_CONTAINER_NAME.set_and_compare(container_name));
    });
}

#[test]
fn test_dynamic_containers_runs() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();

    let container_name = format!("hello_world-on-demand-{}", generate_random_string(20));
    let hello_world = DynamicSpecification::with_repository(&repo, container_name);
    test.provide_container(hello_world);

    test.run(|_ops| async {
        assert!(true);
    });
}

#[test]
fn test_multiple_internal_containers_with_singular_network() {
    let mut test = DockerTest::new()
        .with_default_source(Source::DockerHub)
        .with_network(Network::Singular);

    let repo = "hello-world".to_string();
    let hello_world = TestSuiteSpecification::with_repository(&repo);
    let repo = "hello-world".to_string();
    let hello_world2 = TestSuiteSpecification::with_repository(&repo).set_handle("test");

    test.provide_container(hello_world);
    test.provide_container(hello_world2);

    test.run(|_ops| async {
        assert!(true);
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
