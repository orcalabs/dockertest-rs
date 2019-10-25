use dockertest::waitfor::{ExitedWait, MessageSource, MessageWait, RunningWait, WaitFor};
use dockertest::{Composition, Container, DockerTest, PullPolicy, Source, StartPolicy};
use failure::{format_err, Error};
use futures::future::{self, Future};
use std::rc::Rc;
use tokio::runtime::current_thread;

struct FailWait {}

impl WaitFor for FailWait {
    fn wait_for_ready(
        &self,
        _container: Container,
    ) -> Box<dyn Future<Item = Container, Error = Error>> {
        Box::new(future::err(format_err!("this FailWait shall fail")))
    }
}

// Tests that the RunningWait implementation waits for the container to appear as running.
#[test]
fn test_running_wait_for() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "luca3m/sleep";
    let sleep_container = Composition::with_repository(repo).with_wait_for(Rc::new(RunningWait {
        max_checks: 10,
        check_interval: 1000,
    }));

    test.add_composition(sleep_container);

    test.run(|ops| {
        let handle = ops.handle(repo).expect("failed to get container handle");

        let mut rt = current_thread::Runtime::new().expect("failed to start runtime");

        let is_running = rt
            .block_on(handle.is_running())
            .expect("failed to get container state");

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
    let sleep_container = Composition::with_repository(repo).with_wait_for(Rc::new(ExitedWait {
        max_checks: 10,
        check_interval: 1000,
    }));

    test.add_composition(sleep_container);

    test.run(|ops| {
        let handle = ops.handle(repo).expect("failed to get container handle");

        let mut rt = current_thread::Runtime::new().expect("failed to start runtime");

        let is_running = rt
            .block_on(handle.is_running())
            .expect("failed to get container state");

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
        .with_wait_for(Rc::new(FailWait {}))
        .with_start_policy(StartPolicy::Relaxed);

    test.add_composition(hello_container);

    test.run(|_ops| {
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
        .with_wait_for(Rc::new(FailWait {}))
        .with_start_policy(StartPolicy::Strict);

    test.add_composition(hello_container);

    test.run(|_ops| {
        assert!(false);
    });
}

// Tests that the MessageWait implementation waits for a message to occur in stream
#[test]
fn test_message_wait_for_success_on_stdout() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_container = Composition::with_repository(repo).with_wait_for(Rc::new(MessageWait {
        message: "Hello from Docker!".to_string(),
        source: MessageSource::Stdout,
        timeout: 5,
    }));

    test.add_composition(hello_container);

    test.run(|_ops| {
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
    let hello_container = Composition::with_repository(repo).with_wait_for(Rc::new(MessageWait {
        message: "MESSAGE NOT PRESENT IN OUTPUT".to_string(),
        source: MessageSource::Stdout,
        timeout: 5,
    }));

    test.add_composition(hello_container);

    test.run(|_ops| {
        assert!(false);
    });
}
