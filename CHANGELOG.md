# Changelog
All notable changes to this project will be documented in this file.

## [Unreleased]

## 0.1

### Added
- `DockerTest::source()` is now puplic.
- `RunningContainer::assert_message()` allows one to assert a log outputs presence in the test body.
- `Composition::inject_container_name()` to inject the generated container name as an environmental
    variable for in other containers through its handle.
- Every `DockerTest` instance is ran in its own docker network.
- Control the teardown process of a test through the environmental variable `DOCKERTEST_PRUNE`.
    By setting this to one of the following values:
    * "always": [default] remove everything
    * "never": leave all containers running
    * "stop_on_failure": stop containers on execution failure
    * "running_on_failure": leave containers running on execution failure

    This allows us to more easily debug and inspect test failures by leaving the containers
    stopped/running.

### Changed
- BREAKING: `FnOnce` argument to `DockerTest::run` changed its return type to
    `Future<Output = ()> + Send + 'static`.
    This entails that the inline close must be a single async block.
    ```
    test.run(|ops| async move {
        assert!(true);
    });
    ```
- BREAKING: `FnOnce` argument to `DockerTest::run` changed its provided arguments to an owned
    variant of `DockerOperations`.
- BREAKING: `DockerOperations.handle()` changed to panicking method, directly returning a
    `RunningContainer` instead of a `Result`.

### Removed
- BREAKING: The ability to set host ip port mapping for each container.
    There should be no circumstances where using the docker network directly to communicate
    is insufficient.

## [0.0.4]

Last undocumented release.
