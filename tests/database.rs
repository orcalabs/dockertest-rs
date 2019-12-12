use diesel::pg::PgConnection;
use diesel::prelude::*;
use dockertest::waitfor::{MessageSource, MessageWait};
use dockertest::{Composition, DockerTest, PullPolicy, Source};
use std::rc::Rc;

#[test]
fn test_connect_to_postgres_through_container_ip() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "postgres";
    let postgres = Composition::with_repository(repo).with_wait_for(Rc::new(MessageWait {
        message: "database system is ready to accept connections".to_string(),
        source: MessageSource::Stderr,
        timeout: 20,
    }));

    test.add_composition(postgres);

    test.run(|ops| {
        let container = ops.handle("postgres").expect("retrieve postgres container");
        let ip = container.ip();
        let port = "5432";
        let conn_string = format!("postgres://postgres:postgres@{}:{}", ip, port);
        let pgconn = PgConnection::establish(&conn_string);

        assert!(
            pgconn.is_ok(),
            "failed to establish connection to postgres docker"
        );
    });
}
