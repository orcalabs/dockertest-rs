use dockertest::waitfor::MessageSource;
use dockertest::{Composition, DockerTest};
use test_log::test;

#[test]
fn test_assert_message_in_test_body_succeeds() {
    let mut test = DockerTest::new();
    let composition = Composition::with_repository("dockertest-rs/hello");

    test.add_composition(composition);

    test.run(|ops| async move {
        let hello = ops.handle("dockertest-rs/hello");
        hello
            .assert_message("hello dockertest-rs", MessageSource::Stdout, 5)
            .await;
    });
}

#[test]
#[should_panic]
fn test_assert_message_in_test_body_panics_not_present() {
    let mut test = DockerTest::new();
    let composition = Composition::with_repository("dockertest-rs/hello");

    test.add_composition(composition);

    test.run(|ops| async move {
        let hello = ops.handle("dockertest-rs/hello");
        hello
            .assert_message("not present log message", MessageSource::Stdout, 1)
            .await;
    });
}
