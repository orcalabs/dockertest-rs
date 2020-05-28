# Changelog
All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- `DockerTest::source()` is now puplic.
- `Composition` is now clonable.

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
