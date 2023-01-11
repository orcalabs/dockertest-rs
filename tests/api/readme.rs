//! This is the example test to be featured in the README

use dockertest::{DockerTest, Source::DockerHub, TestBodySpecification};
use std::sync::{Arc, Mutex};

#[test]
fn hello_world_test() {
    // Define our test instance, will pull images from dockerhub.
    let mut test = DockerTest::new().with_default_source(DockerHub);

    // Construct the container specification to be added to the test.
    //
    // A container specification can have multiple properties, depending on how the
    // lifecycle management of the container should be handled by dockertest.
    //
    // For any container specification where dockertest needs to create and start the container,
    // we must provide enough information to construct a composition of
    // an Image configured with provided environment variables, arguments, StartPolicy, etc.
    let hello = TestBodySpecification::with_repository("hello-world");

    // Populate the test instance.
    // The order of compositions added reflect the execution order (depending on StartPolicy).
    test.provide_container(hello);

    let has_ran: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let has_ran_test = has_ran.clone();
    test.run(|ops| async move {
        // A handle to operate on the Container.
        let _container = ops.handle("hello-world");

        // The container is in a running state at this point.
        // Depending on the Image, it may exit on its own (like this hello-world image)
        let mut ran = has_ran_test.lock().unwrap();
        *ran = true;
    });

    let ran = has_ran.lock().unwrap();
    assert!(*ran);
}
