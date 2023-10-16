use std::net::Ipv4Addr;

use dockertest::waitfor::RunningWait;
use dockertest::{DockerTest, Source, TestBodySpecification};
use test_log::test;

use crate::helper::TestHelper;

#[test]
fn test_run_with_no_failure() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let hello_world = TestBodySpecification::with_repository(repo);
    test.provide_container(hello_world);

    test.run(|ops| async move {
        ops.handle("hello-world");
    });
}

#[test]
#[should_panic]
fn test_run_with_failure() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let hello_world = TestBodySpecification::with_repository(repo);
    test.provide_container(hello_world);

    test.run(|_ops| async move {
        panic!();
    });
}

// Tests that we can retrieve the handle of a container by providing the repository as the key
#[test]
fn test_resolve_handle_with_repository_as_key() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_world = TestBodySpecification::with_repository(repo);
    test.provide_container(hello_world);

    test.run(|ops| async move {
        ops.handle("hello-world");
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
    let hello_world = TestBodySpecification::with_repository(repo);
    test.provide_container(hello_world);

    test.run(|ops| async move {
        ops.handle("handle_does_not_exist");
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
    let hello_world = TestBodySpecification::with_repository(repo).set_handle(container_name);
    test.provide_container(hello_world);

    test.run(|ops| async move {
        ops.handle(container_name);
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
    let hello_world = TestBodySpecification::with_repository(repo).set_handle(container_name);
    let hello_world2 = TestBodySpecification::with_repository(repo).set_handle(container_name);
    test.provide_container(hello_world)
        .provide_container(hello_world2);

    test.run(|ops| async move {
        ops.handle(container_name);
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
    let hello_world = TestBodySpecification::with_repository(repo);
    let hello_world2 = TestBodySpecification::with_repository(repo);
    test.provide_container(hello_world)
        .provide_container(hello_world2);

    test.run(|ops| async move {
        ops.handle(repo);
    });
}

// Tests that the RunningWait implementation waits for the container to appear as running.
#[test]
fn test_ip_on_running_container() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let sleep_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(RunningWait {
            max_checks: 10,
            check_interval: 60,
        }));
    test.provide_container(sleep_container);

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
    let mut composition =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(RunningWait {
            max_checks: 10,
            check_interval: 60,
        }));
    composition.modify_port_map(7900, 8500);
    test.provide_container(composition);

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
    let mut composition =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(RunningWait {
            max_checks: 10,
            check_interval: 60,
        }));
    composition.modify_port_map(7900, 8500);
    composition.modify_port_map(7900, 8501);

    test.provide_container(composition);

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
    let hello_world = TestBodySpecification::with_repository(repo);
    test.provide_container(hello_world);

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
    let composition = TestBodySpecification::with_repository(repo).set_publish_all_ports(true);
    test.provide_container(composition);

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
    let sleep_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(RunningWait {
            max_checks: 10,
            check_interval: 60,
        }));
    test.provide_container(sleep_container);

    test.run(|ops| async move {
        let handle = ops.handle(repo);
        // UNSPECIFIED is the default ip-addr.
        // - we simply check that we have populated with something else.
        assert_ne!(handle.ip(), &std::net::Ipv4Addr::UNSPECIFIED);
    });
}

#[test]
fn test_replaces_repository_slashes_with_underscore_for_container_names() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let sleep_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(RunningWait {
            max_checks: 10,
            check_interval: 60,
        }));
    test.provide_container(sleep_container);

    test.run(|ops| async move {
        let handle = ops.handle(repo);
        // container names are {namespace}-{name}-{random-suffix}
        // default namespace is dockertest-rs
        let name = handle.name().split('-').nth(2).unwrap();
        assert_eq!("luca3m_sleep", name);
    });
}

#[test]
fn test_replaces_user_specified_handle_name_containing_slashes_with_underscore() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let name = "test/test";
    let hello_world = TestBodySpecification::with_repository(repo).set_handle(name);
    test.provide_container(hello_world);

    test.run(|ops| async move {
        let handle = ops.handle(name);
        // container names are {namespace}-{name}-{random-suffix}
        // default namespace is dockertest-rs
        let name = handle.name().split('-').nth(2).unwrap();
        assert_eq!("test_test", name);
    });
}

#[test]
fn test_modify_env_sets_environment_variable() {
    let test_helper = TestHelper::new();
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let mut hello_world = TestBodySpecification::with_repository(repo);

    let env_value = "this_is_a_test";
    let env = "TEST123";
    hello_world.modify_env(env, env_value);
    test.provide_container(hello_world);

    test.run(|ops| async move {
        let handle = ops.handle(repo);
        assert_eq!(test_helper.env_value(handle, env).await.unwrap(), env_value);
    });
}

#[test]
fn test_replace_cmd_replaces_command() {
    let test_helper = TestHelper::new();
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";

    let cmd = vec!["/hello".to_string(), "test123".to_string()];
    let hello_world = TestBodySpecification::with_repository(repo).replace_cmd(cmd.clone());

    test.provide_container(hello_world);

    test.run(|ops| async move {
        let handle = ops.handle(repo);
        assert_eq!(test_helper.cmd(handle).await.unwrap(), cmd);
    });
}

// TODO: return a reasonable error message
#[test]
#[should_panic(
    expected = "processing error condition ``Composition::create()` invoked without populating its image through `Image::pull()``"
)]
fn test_non_existing_local_image_fails() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);
    let repo = "this_does_not_exist";
    let non_existing = TestBodySpecification::with_repository(repo);

    test.provide_container(non_existing);

    test.run(|_ops| async move {
        panic!();
    });
}
