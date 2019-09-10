use dockertest_rs::image::{Image, PullPolicy, Remote, Source};
use dockertest_rs::image_instance::ImageInstance;
use dockertest_rs::DockerTest;

#[test]
fn test_run_with_no_failure() {
    let mut test = DockerTest::new();

    let repo = "hello-world".to_string();
    let remote = Source::Remote(Remote::new(&"addr".to_string(), PullPolicy::IfNotPresent));
    let img = Image::with_repository(&repo).source(remote);
    let hello_world = ImageInstance::with_image(img);

    test.add_instance(hello_world);

    test.run(|_ops| {
        assert!(true);
    });
}

#[test]
#[should_panic]
fn test_run_with_failure() {
    let mut test = DockerTest::new();

    let repo = "hello-world".to_string();
    let remote = Source::Remote(Remote::new(&"addr".to_string(), PullPolicy::IfNotPresent));
    let img = Image::with_repository(&repo).source(remote);
    let hello_world = ImageInstance::with_image(img);

    test.add_instance(hello_world);

    test.run(|_ops| {
        assert!(false);
    });
}

#[test]
fn test_multiple_runs() {
    let mut test = DockerTest::new();

    let repo = "hello-world".to_string();
    let remote = Source::Remote(Remote::new(&"addr".to_string(), PullPolicy::IfNotPresent));
    let img = Image::with_repository(&repo).source(remote);
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
