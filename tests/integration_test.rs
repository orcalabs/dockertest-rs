use dockertest_rs::image::{Image, PullPolicy, Source};
use dockertest_rs::image_instance::ImageInstance;
use dockertest_rs::DockerTest;

#[test]
fn test_run_with_no_failure() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let hello_world = ImageInstance::with_image(img);

    test.add_instance(hello_world);

    test.run(|_ops| {
        assert!(true);
    });
}

#[test]
#[should_panic]
fn test_run_with_failure() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let hello_world = ImageInstance::with_image(img);

    test.add_instance(hello_world);

    test.run(|_ops| {
        assert!(false);
    });
}

#[test]
fn test_multiple_runs() {
    let source = Source::DockerHub(PullPolicy::IfNotPresent);
    let mut test = DockerTest::new().with_default_source(source);

    let repo = "hello-world".to_string();
    let img = Image::with_repository(&repo);
    let hello_world = ImageInstance::with_image(img);

    test.add_instance(hello_world);

    test.run(|_ops| {
        assert!(true);
    });

    test.run(|_ops| {
        assert!(true);
    });
}

// FIXME more integration tests
