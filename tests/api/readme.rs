//! This is the example test to be featured in the README

use dockertest::{Composition, DockerTest, Source::DockerHub};
use std::sync::{Arc, Mutex};

#[test]
fn hello_world_test() {
    // Define our test instance, will pull images from dockerhub.
    let mut test = DockerTest::new().with_default_source(DockerHub);

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
