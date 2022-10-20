use dockertest::{Composition, DockerTest, Source};

#[tokio::test]
async fn test_privileged_container() {
    let mut test = DockerTest::new();
    let mut hello_world = Composition::with_repository("dockertest-rs/hello-privileged");

    hello_world.privileged();

    test.add_composition(hello_world);

    test.run_async(|_ops| async {
        assert!(true);
    })
    .await;
}
