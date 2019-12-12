# dockertest-rs

Run docker containers in your Rust integration tests.

This crate provides the following features for your docker testing needs:

* Ensure docker containers are invoked and running according to `WaitFor` strategy.
 * Customize your own or use one of the batteries included.
* Interact with the `RunningContainer` in the test body through its `ip` address.
* Teardown each started container after test is terminated - both successfully and on test failure.

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
let postgres = Composition::with_repository("postgres").with_wait_for(Rc::new(MessageWait {
    message: "database system is ready to accept connections".to_string(),
    source: MessageSource::Stderr,
    timeout: 20,
}));
test.add_composition(postgres);

// Run the test body
test.run(|ops| {
    let container = ops.handle("postgres").expect("retrieve postgres container");
    let ip = container.ip();
    // This is the default postgres serve port
    let port = "5432";
    let conn_string = format!("postgres://postgres:postgres@{}:{}", ip, port);
    let pgconn = PgConnection::establish(&conn_string);

    // Perform your database operations here
    assert!(
        pgconn.is_ok(),
        "failed to establish connection to postgres docker"
    );
});
```

## Development

This library is in its initial inception. Breaking changes are to be expected.

## Testing

Testing this library requires the following:
* docker daemon available on localhost.
* Capable of compiling `diesel` with the postgres feature.

Run the tests with the following command:
```
cargo test -- --test-threads=1
```

Caveats that may fail tests:
* Exited `hello-world` containers on the system. This will currently make removing
old images fail and many tests will fail.
