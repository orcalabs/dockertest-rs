use dockertest::waitfor::{
    async_trait, ExitedWait, MessageSource, MessageWait, RunningWait, WaitFor,
};
use dockertest::{
    Composition, DockerTest, DockerTestError, PendingContainer, PullPolicy, RunningContainer,
    Source, StartPolicy,
};

use bollard::{container::InspectContainerOptions, Docker};
use futures::future::TryFutureExt;
use test_env_log::test;

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
    let client = Docker::connect_with_local_defaults()
        .map_err(|_| DockerTestError::Daemon("connection to local default".to_string()))?;

    let container = client
        .inspect_container(&id, None::<InspectContainerOptions>)
        .map_err(|e| DockerTestError::Recoverable(format!("container did not exist: {}", e)))
        .await?;

    Ok(container.state.running)
}

// Tests that the RunningWait implementation waits for the container to appear as running.
#[test]
fn test_running_wait_for() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let sleep_container = Composition::with_repository(repo).with_wait_for(Box::new(RunningWait {
        max_checks: 10,
        check_interval: 6,
    }));

    test.add_composition(sleep_container);

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
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let sleep_container = Composition::with_repository(repo).with_wait_for(Box::new(ExitedWait {
        max_checks: 10,
        check_interval: 6,
    }));

    test.add_composition(sleep_container);

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
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_container = Composition::with_repository(repo)
        .with_wait_for(Box::new(FailWait {}))
        .with_start_policy(StartPolicy::Relaxed);

    test.add_composition(hello_container);

    test.run(|_ops| async {
        assert!(false);
    });
}

// Check that error on strict container fails the test.
#[test]
#[should_panic]
fn test_wait_for_strict_failed() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_container = Composition::with_repository(repo)
        .with_wait_for(Box::new(FailWait {}))
        .with_start_policy(StartPolicy::Strict);

    test.add_composition(hello_container);

    test.run(|_ops| async {
        assert!(false);
    });
}

// Tests that the MessageWait implementation waits for a message to occur in stream
#[test]
fn test_message_wait_for_success_on_stdout() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_container = Composition::with_repository(repo).with_wait_for(Box::new(MessageWait {
        message: "Hello from Docker!".to_string(),
        source: MessageSource::Stdout,
        timeout: 5,
    }));

    test.add_composition(hello_container);

    test.run(|_ops| async {
        // TODO: Determine how we can assert that this wait for was successful?
        assert!(true);
    });
}

// Tests that the MessageWait implementation fails test when message does not occur.
#[test]
#[should_panic]
fn test_message_wait_for_not_found_on_stream() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_container = Composition::with_repository(repo).with_wait_for(Box::new(MessageWait {
        message: "MESSAGE NOT PRESENT IN OUTPUT".to_string(),
        source: MessageSource::Stdout,
        timeout: 5,
    }));

    test.add_composition(hello_container);

    test.run(|_ops| async {
        assert!(false);
    });
}
