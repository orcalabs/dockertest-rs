# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added

- Added new mechanism to construct Compositions, which are now private. The
  trait `ContainerSpecification` is implemented for each struct that represent a
  different lifecycle management mode for the container it specifies. Thus,
  providing a `StaticManagement` mode is no longer directly exposed, but
  indirectly through the four container specifications:
  `TestBodyContainer`(normal), `TestSuiteContainer`(internal),
  `DynamicContainer`(dynamic), and `ExternalContainer`(external).
- Added `DockerTest::provide_container(c: impl ContainerSpecification)`.
- Added support for privileged containers.

### Changed

- BREAKING `WaitFor` now has a `Debug` trait bound.
- BREAKING Made `Composition` non-public.
- BREAKING Removed `DockerTest::add_composition`. Use
  `DockerTest::provide_container` instead.

## 0.3

### Added

- Added support for static containers that can be used across multiple tests,
  both internally and externally managed.
- Added support for capturing container logs. This is configurable through
  `Composition::with_log_options`.
- Added support to run on an existing docker network. This is configurable
  through `DockerTest::with_external_network`.
- Added support for container network aliases. This is configurable through
  `Composition::with_alias` and `Composition::alias`.
- Added `Composition::publish_all_ports` to publish all exposed ports of a
  container on an ephemeral host port.
- Added optional `tls` feature which when enabled and combined with setting the
  `DOCKER_TLS_VERIFY` env variable the connection with the docker daemon is done
  via TLS over tcp.

### Changed

- BREAKING Added support for custom docker registries. You may now specify a
  `Source` (renamed from `Remote`). Supplying `Source::RegistryWithDockerLogin`
  will extract the appropriate login token from the dockerd daemon credentials
  file, appropriate to the registry you are attempting to pull an image from. If
  `Source::RegistryWithCredentials` is used, you may specify the appropriate
  registry address, username, and password directly.
- BREAKING: `PullPolicy` is now specified on an `Image`, not the `Remote` (now
  `Source`) variant.
- BREAKING: `RunningContainer.ports` changed and renamed to
  `RunningContainer.host_port`. A specific port mapping can now be retrieved
  instead of all of them.

### Fixed

- Anonymous volumes are now cleaned up during teardown.

## 0.2.1

### Changed

- Upgrade tokio from 0.2 to 1.2
- Upgrade bollard from 0.8 t0 0.10

## 0.2.0

### Added

- Ability to communicate with containers when dockertest is ran within a docker
  container environment.
- Added host port mapping support. Usecase is for Windows where intra-container
  communication is not feasable with the same API used on Linux.

### Changed

- Upgraded to bollard 0.8
- Fixed compiling on stable
- BREAKING: Reworked volume API
- Container id generation is now all lowercase.
- BREAKING: Set container ip to localhost when running under windows. Network
  support is lacking on Windows.
- Documentation clarifications and spelling errors.

### Removed

- Diesel dev-dependency.

## 0.1.2

### Added

- Ability to conditionally build docker test images used for running
  DockerTest's own test suite.

## 0.1.1

### Added

- `DockerTest::run` now has an async version: `DockerTest::run_async`. This
  allows us to run async tests that are already scheduled on a runtime, e.g.
  async tests annotated with `#[tokio::test]`.

## 0.1

### Added

- `DockerTest::source()` is now puplic.
- `RunningContainer::assert_message()` allows one to assert a log outputs
  presence in the test body.
- `Composition::inject_container_name()` to inject the generated container name
  as an environmental variable for in other containers through its handle.
- Every `DockerTest` instance is ran in its own docker network.
- Control the teardown process of a test through the environmental variable
  `DOCKERTEST_PRUNE`. By setting this to one of the following values:
  - "always": [default] remove everything
  - "never": leave all containers running
  - "stop_on_failure": stop containers on execution failure
  - "running_on_failure": leave containers running on execution failure

  This allows us to more easily debug and inspect test failures by leaving the
  containers stopped/running.

### Changed

- BREAKING: `FnOnce` argument to `DockerTest::run` changed its return type to
  `Future<Output = ()> + Send + 'static`. This entails that the inline close
  must be a single async block.
  ```
  test.run(|ops| async move {
      assert!(true);
  });
  ```
- BREAKING: `FnOnce` argument to `DockerTest::run` changed its provided
  arguments to an owned variant of `DockerOperations`.
- BREAKING: `DockerOperations.handle()` changed to panicking method, directly
  returning a `RunningContainer` instead of a `Result`.

### Removed

- BREAKING: The ability to set host ip port mapping for each container. There
  should be no circumstances where using the docker network directly to
  communicate is insufficient.

## [0.0.4]

Last undocumented release.
