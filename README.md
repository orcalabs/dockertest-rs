# dockertest-rs

Control docker containers from a rust ingegration test suite. Full fledge management support
with automatic cleanup.

This crate provides the following features for your docker testing needs:

* Ensure docker containers are invoked and running according to `WaitFor` strategy.
 * Customize your own or use one of the batteries included.
* Interact with the `RunningContainer` in the test body through its `ip` address.
* Setup inter-container communication with `inject_container_name` into environment variables.
* Teardown each started container after test is terminated - both successfully and on test failure.
* Concurrent capabilies. All tests are isolated into separate networks per test, with uniquely
generated container names.

See the [crate documentation](https://docs.rs/dockertest) for extensive explainations.

## Example

The canonical example and motivation for this crate can be expressed with the following
use-case example - host a database used by your test in a container, which is fresh between each test.

 ```rust
use diesel::pg::PgConnection;
use diesel::prelude::*;
use dockertest::waitfor::{MessageSource, MessageWait};
use dockertest::{Composition, DockerTest, PullPolicy, Source};
use std::rc::Rc;

// Define our test
let source = Source::DockerHub(PullPolicy::IfNotPresent);
let mut test = DockerTest::new().with_default_source(source);

// Define our Composition - the Image we will start and end up as our RunningContainer
let mut postgres = Composition::with_repository("postgres").with_wait_for(Box::new(MessageWait {
    message: "database system is ready to accept connections".to_string(),
    source: MessageSource::Stderr,
    timeout: 20,
}));
postgres.env("POSTGRES_PASSWORD", "password");
test.add_composition(postgres);

// Run the test body
test.run(|ops| async move {
    let container = ops.handle("postgres");
    let conn_string = format!("postgres://postgres:password@{}:{}", container.ip(), 5432);
    let pgconn = PgConnection::establish(&conn_string);

    // Perform your database operations here
    assert!(
        pgconn.is_ok(),
        "failed to establish connection to postgres docker"
    );
});
```

## Testing

Testing this library requires the following:
* docker daemon available on localhost.
* Capable of compiling `diesel` with the postgres feature.
* Set `DOCKERTEST_BUILD_TEST_IMAGES=1` in your environment, will triger the test images to be built.

Tests are designed to be run in parallel, and should not conflict with existing system images.
Local images are build with repository prefix `dockertest-rs/`.
