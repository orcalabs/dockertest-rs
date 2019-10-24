use diesel::pg::PgConnection;
use diesel::prelude::*;
use dockertest::image::{PullPolicy, Source};
use dockertest::waitfor::{MessageSource, MessageWait};
use dockertest::{Composition, DockerTest};
use std::rc::Rc;

#[ignore]
#[test]
fn test_connect_to_postgres_through_host_port() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "postgres";
    let mut postgres = Composition::with_repository(repo).wait_for(Rc::new(MessageWait {
        message: "database system is ready to accept connections".to_string(),
        source: MessageSource::Stderr,
        timeout: 20,
    }));
    postgres.port_map(5432, 5432);

    test.add_composition(postgres);

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
}
