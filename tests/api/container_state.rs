use crate::helper::TestHelper;
use dockertest::waitfor::RunningWait;
use dockertest::{ContainerState, DockerTest, Source, TestBodySpecification};
use test_log::test;

#[test]
fn test_pause_container() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let sleep_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(RunningWait {
            max_checks: 10,
            check_interval: 6,
        }));

    test.provide_container(sleep_container);

    let helper = TestHelper::new();

    test.run(|ops| async move {
        let handle = ops.handle(repo);

        handle.pause().await;

        let actual_state = helper.container_state(handle.name()).await;

        assert_eq!(actual_state, ContainerState::Paused);
    });
}

#[test]
fn test_kill_container() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let sleep_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(RunningWait {
            max_checks: 10,
            check_interval: 6,
        }));

    test.provide_container(sleep_container);

    let helper = TestHelper::new();

    test.run(|ops| async move {
        let handle = ops.handle(repo);

        handle.kill().await;

        let actual_state = helper.container_state(handle.name()).await;

        assert_eq!(actual_state, ContainerState::Exited);
    });
}

#[test]
fn test_start_container() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let sleep_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(RunningWait {
            max_checks: 10,
            check_interval: 6,
        }));

    test.provide_container(sleep_container);

    let helper = TestHelper::new();

    test.run(|ops| async move {
        let handle = ops.handle(repo);

        handle.pause().await;
        let actual_state = helper.container_state(handle.name()).await;
        assert_eq!(actual_state, ContainerState::Paused);

        handle.unpause().await;
        let actual_state = helper.container_state(handle.name()).await;
        assert_eq!(actual_state, ContainerState::Running);
    });
}

#[test]
#[should_panic]
fn test_double_pausing_container_panics() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let sleep_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(RunningWait {
            max_checks: 10,
            check_interval: 6,
        }));

    test.provide_container(sleep_container);

    test.run(|ops| async move {
        let handle = ops.handle(repo);

        handle.pause().await;
        handle.pause().await;
    });
}

#[test]
#[should_panic]
fn test_double_killing_container_panics() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let sleep_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(RunningWait {
            max_checks: 10,
            check_interval: 6,
        }));

    test.provide_container(sleep_container);

    test.run(|ops| async move {
        let handle = ops.handle(repo);

        handle.kill().await;
        handle.kill().await;
    });
}

#[test]
#[should_panic]
fn test_unpausing_non_paused_container_panics() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let sleep_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(RunningWait {
            max_checks: 10,
            check_interval: 6,
        }));

    test.provide_container(sleep_container);

    test.run(|ops| async move {
        let handle = ops.handle(repo);

        handle.unpause().await;
    });
}
