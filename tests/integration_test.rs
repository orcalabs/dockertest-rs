use dockertest_rs::image::{Image, PullPolicy, Source};
use dockertest_rs::image_instance::ImageInstance;
use dockertest_rs::DockerTest;

#[test]
fn test_run_with_no_failure() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let hello_world = ImageInstance::with_image(img);

    test.add_instance(hello_world);

    test.run(|_ops| {
        assert!(true);
    });
}

#[test]
#[should_panic]
fn test_run_with_failure() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let hello_world = ImageInstance::with_image(img);

    test.add_instance(hello_world);

    test.run(|_ops| {
        assert!(false);
    });
}

#[test]
fn test_multiple_runs() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let hello_world = ImageInstance::with_image(img);

    test.add_instance(hello_world);

    test.run(|_ops| {
        assert!(true);
    });

    test.run(|_ops| {
        assert!(true);
    });
}

// Tests that we can retrieve the handle of a container by providing the repository as the key
#[test]
fn test_resolve_handle_with_repository_as_key() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let img = Image::with_repository(&repo);
    let hello_world = ImageInstance::with_image(img);

    test.add_instance(hello_world);

    test.run(|ops| {
        let handle = ops.handle(repo);
        assert!(handle.is_ok(), "failed to retrieve handle for container");
    });
}

// Tests that we fail to retrieve a handle for a container with an invalid key
#[test]
fn test_resolve_handle_with_invalid_key() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let img = Image::with_repository(&repo);
    let hello_world = ImageInstance::with_image(img);

    test.add_instance(hello_world);

    test.run(|ops| {
        let handle = ops.handle("not_an_existing_key");
        assert!(
            handle.is_err(),
            "should fail to retrieve handle for container with non-existing key"
        );
    });
}

// Tests that we can retrieve handle to container by using the user provided container name as the
// key
#[test]
fn test_resolve_handle_with_user_provided_container_name_as_key() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let container_name = "this_is_a_container_name";
    let img = Image::with_repository(&repo);
    let hello_world = ImageInstance::with_image(img).with_container_name(container_name);

    test.add_instance(hello_world);

    test.run(|ops| {
        let handle = ops.handle(container_name);
        assert!(handle.is_ok(), "failed to retrieve handle for container");
    });
}

// Tests that we fail to retrieve a handle for a container when multiple containers have the same
// user provided container name
#[test]
fn test_resolve_handle_with_identical_user_provided_container_name() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let container_name = "this_is_a_container_name";
    let hello_world = ImageInstance::with_repository(repo).with_container_name(container_name);
    let hello_world2 = ImageInstance::with_repository(repo).with_container_name(container_name);

    test.add_instance(hello_world);
    test.add_instance(hello_world2);

    test.run(|ops| {
        let handle = ops.handle(container_name);
        assert!(handle.is_err(),
                "should fail to retrieve handle for container when there exists multiple containers with the same user_provided_container_name");
    });
}

// Tests that we fail to retrieve a handle for a container when multiple containers have the same
// repository name
#[test]
fn test_resolve_handle_with_identical_repository() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_world = ImageInstance::with_repository(repo);
    let hello_world2 = ImageInstance::with_repository(repo);

    test.add_instance(hello_world);
    test.add_instance(hello_world2);

    test.run(|ops| {
        let handle = ops.handle(repo);
        assert!(handle.is_err(),
                "should fail to retrieve handle for container when there exists multiple containers with the same repository name and no user_provided_container_name");
    });
}
