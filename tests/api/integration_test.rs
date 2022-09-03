use std::net::Ipv4Addr;

use dockertest::waitfor::RunningWait;
use dockertest::{Composition, DockerTest, Image, Source};
use test_log::test;

#[test]
fn test_run_with_no_failure() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let hello_world = Composition::with_image(img);

    test.add_composition(hello_world);

    test.run(|_ops| async {
        assert!(true);
    });
}

#[test]
#[should_panic]
fn test_run_with_failure() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let hello_world = Composition::with_image(img);

    test.add_composition(hello_world);

    test.run(|_ops| async {
        assert!(false);
    });
}

// Tests that we can retrieve the handle of a container by providing the repository as the key
#[test]
fn test_resolve_handle_with_repository_as_key() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let img = Image::with_repository(&repo);
    let hello_world = Composition::with_image(img);

    test.add_composition(hello_world);

    test.run(|ops| async move {
        let _ = ops.handle(repo);
    });
}

// Tests that we fail to retrieve a handle for a container with an invalid key
#[test]
#[should_panic(
    expected = "test body failure `container with handle 'handle_does_not_exist' not found`"
)]
fn test_resolve_handle_with_invalid_key() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let img = Image::with_repository(&repo);
    let hello_world = Composition::with_image(img);

    test.add_composition(hello_world);

    test.run(|ops| async move {
        let _ = ops.handle("handle_does_not_exist");
    });
}

// Tests that we can retrieve handle to container by using the user provided container name as the
// key
#[test]
fn test_resolve_handle_with_user_provided_container_name_as_key() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let container_name = "this_is_a_container_name";
    let img = Image::with_repository(&repo);
    let hello_world = Composition::with_image(img).with_container_name(container_name);

    test.add_composition(hello_world);

    test.run(|ops| async move {
        let _ = ops.handle(container_name);
    });
}

// Tests that we fail to retrieve a handle for a container when multiple containers have the same
// user provided container name
#[test]
#[should_panic(
    expected = "test body failure `handle 'this_is_a_container_name' defined multiple times`"
)]
fn test_resolve_handle_with_identical_user_provided_container_name() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let container_name = "this_is_a_container_name";
    let hello_world = Composition::with_repository(repo).with_container_name(container_name);
    let hello_world2 = Composition::with_repository(repo).with_container_name(container_name);

    test.add_composition(hello_world);
    test.add_composition(hello_world2);

    test.run(|ops| async move {
        let _ = ops.handle(container_name);
    });
}

// Tests that we fail to retrieve a handle for a container when multiple containers have the same
// repository name
#[test]
#[should_panic(expected = "test body failure `handle 'hello-world' defined multiple times`")]
fn test_resolve_handle_with_identical_repository() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_world = Composition::with_repository(repo);
    let hello_world2 = Composition::with_repository(repo);

    test.add_composition(hello_world);
    test.add_composition(hello_world2);

    test.run(|ops| async move {
        let _ = ops.handle(repo);
    });
}

// Tests that the RunningWait implementation waits for the container to appear as running.
#[test]
fn test_ip_on_running_container() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let sleep_container = Composition::with_repository(repo).with_wait_for(Box::new(RunningWait {
        max_checks: 10,
        check_interval: 60,
    }));

    test.add_composition(sleep_container);

    test.run(|ops| async move {
        let handle = ops.handle(repo);
        // UNSPECIFIED is the default ip-addr.
        // - we simply check that we have populated with something else.
        assert_ne!(handle.ip(), &std::net::Ipv4Addr::UNSPECIFIED);
    });
}

#[test]
fn test_host_port_returns_correct_host_port_when_using_port_mapping() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let mut composition = Composition::with_repository(repo).with_wait_for(Box::new(RunningWait {
        max_checks: 10,
        check_interval: 60,
    }));

    composition.port_map(7900, 8500);

    test.add_composition(composition);

    test.run(|ops| async move {
        let handle = ops.handle(repo);
        let ports = handle.host_port(7900).unwrap();

        assert_eq!(8500, ports.1);
        assert_eq!(Ipv4Addr::LOCALHOST, ports.0);
    });
}

#[test]
fn test_host_port_returns_the_last_port_mapping_if_multiple_mappings_applied_to_same_container_port(
) {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let mut composition = Composition::with_repository(repo).with_wait_for(Box::new(RunningWait {
        max_checks: 10,
        check_interval: 60,
    }));

    composition.port_map(7900, 8500);
    composition.port_map(7900, 8501);

    test.add_composition(composition);

    test.run(|ops| async move {
        let handle = ops.handle(repo);
        let ports = handle.host_port(7900).unwrap();

        assert_eq!(8501, ports.1);
        assert_eq!(Ipv4Addr::LOCALHOST, ports.0);
    });
}

#[test]
fn test_host_port_returns_none_if_the_port_is_not_mapped() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_world = Composition::with_repository(repo);

    test.add_composition(hello_world);

    test.run(|ops| async move {
        let handle = ops.handle(repo);
        assert!(handle.host_port(7900).is_none());
    });
}

#[test]
fn test_host_port_returns_ports_exposed_by_publish_all() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "dockertest-rs/expose_ports";
    let mut composition = Composition::with_repository(repo);
    composition.publish_all_ports();
    test.add_composition(composition);

    test.run(|ops| async move {
        let handle = ops.handle(repo);

        let ports = handle.host_port(8080).unwrap();
        assert_eq!(Ipv4Addr::UNSPECIFIED, ports.0);

        let ports = handle.host_port(9000).unwrap();
        assert_eq!(Ipv4Addr::UNSPECIFIED, ports.0);

        let ports = handle.host_port(4567).unwrap();
        assert_eq!(Ipv4Addr::UNSPECIFIED, ports.0);
    });
}

#[test]
fn test_ip_on_running_container_with_namespaced_instance() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new()
        .with_default_source(source)
        .with_namespace("test");

    let repo = "luca3m/sleep";
    let sleep_container = Composition::with_repository(repo).with_wait_for(Box::new(RunningWait {
        max_checks: 10,
        check_interval: 60,
    }));

    test.add_composition(sleep_container);

    test.run(|ops| async move {
        let handle = ops.handle(repo);
        // UNSPECIFIED is the default ip-addr.
        // - we simply check that we have populated with something else.
        assert_ne!(handle.ip(), &std::net::Ipv4Addr::UNSPECIFIED);
    });
}
