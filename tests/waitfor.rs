use dockertest::container::Container;
use dockertest::image::{PullPolicy, Source};
use dockertest::waitfor::{ExitedWait, MessageSource, MessageWait, RunningWait, WaitFor};
use dockertest::{Composition, DockerTest, StartPolicy};
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
    let sleep_container = Composition::with_repository(repo).wait_for(Rc::new(RunningWait {
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
    let sleep_container = Composition::with_repository(repo).wait_for(Rc::new(ExitedWait {
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
        .wait_for(Rc::new(FailWait {}))
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
        .wait_for(Rc::new(FailWait {}))
        .with_start_policy(StartPolicy::Strict);

    test.add_composition(hello_container);

    test.run(|_ops| {
        assert!(false);
    });
}
