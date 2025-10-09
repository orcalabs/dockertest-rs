use bollard::secret::ContainerStateStatusEnum;
use dockertest::waitfor::{MessageSource, MessageWait};
use dockertest::{DockerTest, Source, TestBodySpecification};
use test_log::test;

use crate::helper::TestHelper;

#[test]
fn test_tmpfs_succeeds() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let mut hello_world = TestBodySpecification::with_repository("postgres")
        .set_wait_for(Box::new(MessageWait {
            message: "database system is ready to accept connections".to_string(),
            source: MessageSource::Stdout,
            timeout: 60,
        }))
        .set_log_options(None);

    let mount_point = "/var/lib/pg/data";
    hello_world.modify_env("POSTGRES_HOST_AUTH_METHOD", "trust");
    hello_world.modify_env("PGDATA", mount_point);
    hello_world.tmpfs(mount_point);

    test.provide_container(hello_world);

    let helper = TestHelper::new();

    test.run(|ops| async move {
        let handle = ops.handle("postgres");
        assert_eq!(
            helper.container_status(handle).await,
            ContainerStateStatusEnum::RUNNING
        );
        assert!(helper.has_tmpfs_mount_point(handle, mount_point).await);
    });
}
