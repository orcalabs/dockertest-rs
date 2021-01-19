# dockertest-rs

Control docker containers from a Rust integration test suite. Full fledge management support
with automatic cleanup.

This crate provides the following features for your docker testing needs:

* Ensure docker containers are invoked and running according to `WaitFor` strategy.
 * Customize your own or use one of the batteries included.
* Interact with the `RunningContainer` in the test body through its `ip` address.
* Setup inter-container communication with `inject_container_name` into environment variables.
* Teardown each started container after test is terminated - both successfully and on test failure.
* Concurrent capabilities. All tests are isolated into separate networks per test, with uniquely
generated container names.

See the [crate documentation](https://docs.rs/dockertest) for extensive explanations.

## Example

This is a trivial example showing of the general structure of a naive test.

 ```rust
use dockertest::{Composition, DockerTest};
use std::sync::{Arc, Mutex};

#[test]
fn hello_world_test() {
    // Define our test instance
    let mut test = DockerTest::new();

    // Construct the Composition to be added to the test.
    // A Composition is an Image configured with environment, arguments, StartPolicy, etc.,
    // seen as an instance of the Image prior to constructing the Container.
    let hello = Composition::with_repository("hello-world");

    // Populate the test instance.
    // The order of compositions added reflect the execution order (depending on StartPolicy).
    test.add_composition(hello);

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
```

## Testing

Testing this library requires the following:
* docker daemon available on localhost.
* Set `DOCKERTEST_BUILD_TEST_IMAGES=1` in your environment, will trigger the test images to be built.

Tests are designed to be run in parallel, and should not conflict with existing system images.
Local images are build with repository prefix `dockertest-rs/`.

## Running dockertest inside docker
To run `dockertest` inside docker you will have to set the following environment variable:
- `DOCKERTEST_CONTAINER_ID_INJECT_TO_NETWORK=your_container_id/name`

`DOCKERTEST_CONTAINER_ID_INJECT_TO_NETWORK` has to be set to the ID or name of the container `dockertest` is running in.
