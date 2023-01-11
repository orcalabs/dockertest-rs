use dockertest::{DockerTest, Source, TestBodySpecification};

#[tokio::test]
async fn test_with_tokio_test() {
    let source = Source::DockerHub;
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world";
    let hello_world = TestBodySpecification::with_repository(repo);
    test.provide_container(hello_world);

    test.run_async(|_ops| async {
        assert!(true);
    })
    .await;
}
