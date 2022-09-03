//! Configure a DockerTest to run.

use crate::composition::Composition;
use crate::image::Source;
use crate::runner::{DockerOperations, Runner};
use crate::DockerTestError;

use futures::future::Future;
use tokio::runtime::Runtime;
use tracing::{event, span, Instrument, Level};

/// The internal configuration object of a DockerTest instance.
pub struct DockerTest {
    /// All Compositions that have been added to this test run.
    /// They are stored in the order they where added by `add_composition`.
    pub(crate) compositions: Vec<Composition>,
    /// The namespace of all started containers,
    /// this is essentially only a prefix on each container.
    /// Used to more easily identify which containers was
    /// started by DockerTest.
    pub(crate) namespace: String,
    /// The default pull source to use for all images.
    /// Images with a specified source will override this default.
    pub(crate) default_source: Source,
    /// Retrieved internally by an env variable the user has to set.
    /// Will only be used in environments where dockertest itself is running inside a container.
    pub(crate) container_id: Option<String>,
    /// Network configuration, defaults to [Network::Singular] if not specified by
    /// user.
    pub(crate) network: Network,
}

/// Describes the docker network configuration for [DockerTest]
/// The default value if not provided is [Network::Singular]
#[derive(Debug)]
pub enum Network {
    /// A singular docker network named `dockertest` is created and shared by all dockertest instances and the network
    /// itself will never be deleted. It will be reused instead of created if it already exists.
    ///
    /// All static containers with the policy [crate::composition::StaticManagementPolicy::External] or [crate::composition::StaticManagementPolicy::Dynamic]
    /// will never be de-attached from this network.
    ///
    /// In the event that multiple `dockertest` networks exists the most recent created network is
    /// used. This might occur if multiple tests are running in parallel while there are no
    /// pre-existing `dockertest` network.
    Singular,
    /// Test will use an externally managed docker network.
    ///
    /// All created containers will attach itself to the existing, externally managed network.
    ///
    /// If a container is created with a [crate::composition::StaticManagementPolicy::External],
    /// it is assumed that the container is already part of this network.
    ///
    /// For [crate::composition::StaticManagementPolicy::Internal], the container will be included
    /// into the network before test starts, and dropped once the statically managed container
    /// is removed.
    External(String),
    /// Each dockertest instance creates its own isolated docker network which will be deleted upon
    /// test completion.
    Isolated,
}

impl DockerTest {
    /// Configure a new instance of [DockerTest].
    pub fn new() -> Self {
        Self {
            default_source: Source::Local,
            compositions: Vec::new(),
            namespace: "dockertest-rs".to_string(),
            container_id: None,
            network: Network::Singular,
        }
    }

    /// Sets the default source for all images.
    ///
    /// All images without a specified source will be pulled from the default source.
    /// DockerTest will default to Local if no default source is provided.
    pub fn with_default_source(self, default_source: Source) -> Self {
        Self {
            default_source,
            ..self
        }
    }

    /// Sets the namespace for all containers created by [DockerTest].
    ///
    /// All container names will be prefixed with this namespace.
    /// DockerTest defaults to the namespace "dockertest-rs".
    pub fn with_namespace<T: ToString>(self, name: T) -> Self {
        Self {
            namespace: name.to_string(),
            ..self
        }
    }

    /// Sets the network configuration
    pub fn with_network(self, network: Network) -> Self {
        Self { network, ..self }
    }

    /// Add a Composition to this DockerTest.
    pub fn add_composition(&mut self, instance: Composition) {
        self.compositions.push(instance);
    }

    /// Retrieve the default source for Images unless explicitly specified per Image.
    pub fn source(&self) -> &Source {
        &self.default_source
    }

    /// Execute the test with the constructed environment in full operation.
    ///
    /// # Synchronous
    /// This synchronous version of executes the test with its own runtime.
    // NOTE(clippy): tracing generates cognitive complexity due to macro expansion.
    #[allow(clippy::cognitive_complexity)]
    pub fn run<T, Fut>(self, test: T)
    where
        T: FnOnce(DockerOperations) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let span = span!(Level::ERROR, "run");
        let _guard = span.enter();

        // Allocate a new runtime for this test.
        let rt = match Runtime::new() {
            Ok(r) => r,
            Err(e) => {
                event!(Level::ERROR, "failed to allocate tokio runtime: {}", e);
                panic!("{}", e);
            }
        };

        let runner = rt.block_on(Runner::new(self));
        process_run(rt.block_on(runner.run_impl(test).in_current_span()))
    }

    /// Async version of [DockerTest::run].
    ///
    /// # Asynchronous
    /// This version allows the caller to provide the runtime to execute this test within.
    /// This can be useful if the test executable is wrapped with a runtime macro, e.g.,
    /// `#[tokio::test]`.
    pub async fn run_async<T, Fut>(self, test: T)
    where
        T: FnOnce(DockerOperations) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let span = span!(Level::ERROR, "run");
        let _guard = span.enter();

        let runner = Runner::new(self).await;
        process_run(runner.run_impl(test).in_current_span().await);
    }
}

impl Default for DockerTest {
    fn default() -> Self {
        Self::new()
    }
}

fn process_run(result: Result<(), DockerTestError>) {
    match result {
        Ok(_) => event!(Level::DEBUG, "dockertest successfully executed"),
        Err(e) => {
            event!(
                Level::ERROR,
                "internal dockertest condition failure: {:?}",
                e
            );
            event!(Level::WARN, "dockertest failure");
            panic!("{}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{DockerTest, Source};

    // The default DockerTest constructor produces a valid instance with the correct values set
    #[test]
    fn test_default_constructor() {
        let test = DockerTest::new();
        assert_eq!(
            test.compositions.len(),
            0,
            "should not have any strict instances after creation"
        );

        assert_eq!(
            test.namespace,
            "dockertest-rs".to_string(),
            "default namespace was not set correctly"
        );

        let equal = match *test.source() {
            Source::Local => true,
            _ => false,
        };

        assert!(equal, "source not set to local by default");
    }

    // The `with_namespace` builder method sets the namespace correctly
    #[test]
    fn test_with_namespace() {
        let namespace = "this_is_a_test_namespace".to_string();
        let test = DockerTest::new().with_namespace(&namespace);

        assert_eq!(
            test.namespace, namespace,
            "default namespace was not set correctly"
        );
    }

    // The `with_default_source` builder method sets the default_source_correctly
    #[test]
    fn test_with_default_source() {
        let test = DockerTest::new().with_default_source(Source::DockerHub);

        let equal = match test.default_source {
            Source::DockerHub => true,
            _ => false,
        };

        assert!(equal, "default_source was not set correctly");
    }
}
