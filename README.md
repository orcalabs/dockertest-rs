# dockertest-rs

Run docker containers in your Rust integration tests.

This crate provides the following features for your docker testing needs:

* Ensure one or more docker containers are running before your test executes.
* Teardown each started container after test is terminated (both successfully and on test failure).
* Control origin and management of `Image`s used for your docker containers and the pull policy thereof.
Supported sources are:
  * Local - must be present on the host machine.
  * DockerHub - fetch from the offical DockerHub.
  * Custom - custom docker registry. (Currently not implemented)

## Architecture

This shows the various stages of components in dockertest-rs.

```Image -> Composition -> PendingContainer -> RunningContainer```

An `Image` represents a built image from docker that we may attempt to start a container of.

A `Composition` is a specific instance of an `Image`, with its runtime parameters supplied.

The `PendingContainer` represents a docker container who is either built or running,
but not yet in the control of the test body.

Once the test body executes, all containers will be available as `RunningContainer`s and
all operations present on this object will be available to the test body.

## Example

 ```rust
use diesel::pg::PgConnection;
use diesel::prelude::*;
use dockertest::image::{PullPolicy, Source};
use dockertest::{Composition, DockerTest};

let source = Source::DockerHub(PullPolicy::IfNotPresent);
let mut test = DockerTest::new().with_default_source(source);

let repo = "postgres";
let postgres = Composition::with_repository(repo);

test.add_instance(postgres);

test.run(|ops| {
    let container = ops.handle("postgres").expect("retrieve postgres container");
    let host_port = container.host_port(5432);
    let conn_string = format!("postgres://postgres:postgres@localhost:{}", host_port);
    let pgconn = PgConnection::establish(&conn_string);

    assert!(
        pgconn.is_ok(),
        "failed to establish connection to postgres docker"
    );
});
```

## Development

This library is in its initial inception. Breaking changes are to be expected.

## TODO:
* Document limits when spawning stuff in test body.
* Document handle concept.
* Break up Container into PendingContainer and RunningContainer.
* Document and implement port mapping
