use dockertest::utils::connect_with_local_or_tls_defaults;
use dockertest::waitfor::{
    async_trait, ExitedWait, MessageSource, MessageWait, RunningWait, WaitFor,
};
use dockertest::{
    DockerTest, DockerTestError, PendingContainer, RunningContainer, Source, StartPolicy,
    TestBodySpecification,
};

use bollard::container::InspectContainerOptions;
use futures::future::TryFutureExt;
use test_log::test;

#[derive(Clone, Debug)]
struct FailWait {}

#[async_trait]
impl WaitFor for FailWait {
    async fn wait_for_ready(
        &self,
        _container: PendingContainer,
    ) -> Result<RunningContainer, DockerTestError> {
        Err(DockerTestError::Processing(
            "this FailWait shall fail".to_string(),
        ))
    }
}

/// Returns whether the container is in a running state.
pub async fn is_running(id: String) -> Result<bool, DockerTestError> {
    let client = connect_with_local_or_tls_defaults()?;

    let container = client
        .inspect_container(&id, None::<InspectContainerOptions>)
        .map_err(|e| DockerTestError::Recoverable(format!("container did not exist: {}", e)))
        .await?;

    Ok(container.state.unwrap().running.unwrap())
}

// Tests that the RunningWait implementation waits for the container to appear as running.
#[test]
fn test_running_wait_for() {
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

        let is_running = is_running(handle.id().to_string()).await.unwrap();

        assert!(
            is_running,
            "container should be running when using the RunningWait waiting strategy"
        );
    });
}

// Tests that the ExitedWait implementation waits for the container to reach an exit status.
#[test]
fn test_exit_wait_for() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let sleep_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(ExitedWait {
            max_checks: 10,
            check_interval: 6,
        }));

    test.provide_container(sleep_container);

    test.run(|ops| async move {
        let handle = ops.handle(repo);

        let is_running = is_running(handle.id().to_string()).await.unwrap();

        assert!(
            !is_running,
            "container should not be running when using the ExitWait waiting strategy"
        );
    });
}

// Check that error on relaxed container fails the test.
#[test]
#[should_panic]
fn test_wait_for_relaxed_failed() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_container = TestBodySpecification::with_repository(repo)
        .set_wait_for(Box::new(FailWait {}))
        .set_start_policy(StartPolicy::Relaxed);

    test.provide_container(hello_container);

    test.run(|ops| async move {
        ops.handle("hello-world");
    });
}

// Check that error on strict container fails the test.
#[test]
#[should_panic]
fn test_wait_for_strict_failed() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_container = TestBodySpecification::with_repository(repo)
        .set_wait_for(Box::new(FailWait {}))
        .set_start_policy(StartPolicy::Strict);

    test.provide_container(hello_container);

    test.run(|ops| async move {
        ops.handle("hello-world");
    });
}

// Tests that the MessageWait implementation waits for a message to occur in stream
#[test]
fn test_message_wait_for_success_on_stdout() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(MessageWait {
            message: "Hello from Docker!".to_string(),
            source: MessageSource::Stdout,
            timeout: 5,
        }));

    test.provide_container(hello_container);

    test.run(|ops| async move {
        // TODO: Determine how we can assert that this wait for was successful?
        ops.handle("hello-world");
    });
}

// Tests that the MessageWait implementation fails test when message does not occur.
#[test]
#[should_panic]
fn test_message_wait_for_not_found_on_stream() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_container =
        TestBodySpecification::with_repository(repo).set_wait_for(Box::new(MessageWait {
            message: "MESSAGE NOT PRESENT IN OUTPUT".to_string(),
            source: MessageSource::Stdout,
            timeout: 5,
        }));

    test.provide_container(hello_container);

    test.run(|ops| async move {
        ops.handle("hello-world");
    });
}
