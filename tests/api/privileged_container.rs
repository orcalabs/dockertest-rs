use dockertest::{DockerTest, TestBodySpecification};

#[tokio::test]
async fn test_privileged_container() {
    let mut test = DockerTest::new();
    let mut hello_world = TestBodySpecification::with_repository("dockertest-rs/hello-privileged");

    hello_world.privileged(true);

    test.provide_container(hello_world);

    test.run_async(|ops| async move {
        ops.handle("dockertest-rs/hello-privileged");
    })
    .await;
}
