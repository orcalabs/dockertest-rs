use diesel::pg::PgConnection;
use diesel::prelude::*;
use dockertest::waitfor::{MessageSource, MessageWait};
use dockertest::{Composition, DockerTest, PullPolicy, Source};
use test_env_log::test;

#[test]
fn test_connect_to_postgres_through_container_ip() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "postgres";
    let mut postgres = Composition::with_repository(repo).with_wait_for(Box::new(MessageWait {
        message: "database system is ready to accept connections".to_string(),
        source: MessageSource::Stderr,
        timeout: 20,
    }));
    postgres.env("POSTGRES_PASSWORD", "password");

    test.add_composition(postgres);

    test.run(|ops| async move {
        let container = ops.handle("postgres");
        let conn_string = format!("postgres://postgres:password@{}:{}", container.ip(), 5432);
        let pgconn = PgConnection::establish(&conn_string);

        assert!(
            pgconn.is_ok(),
            "failed to establish connection to postgres docker"
        );
    });
}
