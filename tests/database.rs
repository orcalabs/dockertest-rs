use diesel::pg::PgConnection;
use diesel::prelude::*;
use dockertest::image::{PullPolicy, Source};
use dockertest::image_instance::ImageInstance;
use dockertest::DockerTest;

#[ignore]
#[test]
fn test_connect_to_postgres_through_host_port() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "postgres";
    let postgres = ImageInstance::with_repository(repo);

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
}
