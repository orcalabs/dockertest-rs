use dockertest::{Composition, DockerTest, Image, PullPolicy, Source};

#[tokio::test]
async fn test_with_tokio_test() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let hello_world = Composition::with_image(img);

    test.add_composition(hello_world);

    test.run_async(|_ops| async {
        assert!(true);
    })
    .await;
}
