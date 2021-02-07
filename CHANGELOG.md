# Changelog
All notable changes to this project will be documented in this file.

## [Unreleased]

## 0.2.0

### Added
- Ability to communicate with containers when dockertest is ran within a docker container environment.
- Added host port mapping support. Usecase is for Windows where intra-container communication is not feasable with the same API used on Linux.

### Changed
- Upgraded to bollard 0.8
- Fixed compiling on stable
- BREAKING: Reworked volume API
- Container id generation is now all lowercase.
- BREAKING: Set container ip to localhost when running under windows. Network support is lacking on Windows.
- Documentation clarifications and spelling errors.

### Removed
- Diesel dev-dependency.

## 0.1.2

### Added
- Ability to conditionally build docker test images used for running DockerTest's own test suite.

## 0.1.1

### Added
- `DockerTest::run` now has an async version: `DockerTest::run_async`.
    This allows us to run async tests that are already scheduled on a runtime, e.g. async tests
    annotated with `#[tokio::test]`.

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
