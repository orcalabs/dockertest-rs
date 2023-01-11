//! Configure a DockerTest to run.

use crate::composition::Composition;
use crate::image::Source;
use crate::runner::{DockerOperations, Runner};
use crate::specification::ContainerSpecification;
use crate::DockerTestError;

use futures::future::Future;
use tokio::runtime::Runtime;
use tracing::{event, span, Instrument, Level};

/// The main entry point to specify a test.
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

/// Configure how the docker network should be applied to the containers within this test.
///
/// The default value for a [DockerTest], if not provided, is [Network::Singular].
#[derive(Debug)]
pub enum Network {
    /// A single statically named network, with the namespace of the [DockerTest] as a prefix.
    ///
    /// This network will be shared between each test using the same namespace, and the network
    /// itself will never be deleted. This network is created on demand, but is never deleted.
    /// This is to facilitate reuse between tests, to avoid the cost of creating a new
    /// docker network when not necessary.
    ///
    /// In the event that multiple networks matching the same name exists, the most recently
    /// created network is selected. This situation might occur when multiple tests are running
    /// in parallel while there are no pre-existing network. No locking is performed to avoid
    /// this race condition, since it has no impact on test performance.
    Singular,
    /// Test will use an externally managed docker network.
    ///
    /// All created containers will attach itself to the existing, externally managed network.
    External(String),
    /// Each [DockerTest] instance will create and manage its own isolated docker network.
    ///
    /// The network will be deleted once the test body exits.
    Isolated,
}

impl DockerTest {
    /// Start the configuration process of a new [DockerTest] instance.
    pub fn new() -> Self {
        Self {
            default_source: Source::Local,
            compositions: Vec::new(),
            namespace: "dockertest-rs".to_string(),
            container_id: None,
            network: Network::Singular,
        }
    }

    /// Sets the default [Source] for all [Image]s.
    ///
    /// All images without a specified source will be pulled from the default source.
    /// DockerTest will default to [Source::Local] if not configured.
    ///
    /// [Image]: crate::image::Image
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

    /// Append a container specification as part of this specific test.
    ///
    /// The order of which container specifications are added to DockerTest is significant
    /// for the start-up order for strict order dependencies.
    ///
    /// Please refer to one of the following variants to construct your container specification:
    /// * [TestBodySpecification]
    /// * [DynamicSpecification]
    /// * [ExternalSpecification]
    ///
    /// [TestBodySpecification]: crate::specification::TestBodySpecification
    /// [DynamicSpecification]: crate::specification::DynamicSpecification
    /// [ExternalSpecification]: crate::specification::ExternalSpecification
    pub fn provide_container(
        &mut self,
        specification: impl ContainerSpecification,
    ) -> &mut DockerTest {
        let composition = specification.into_composition();
        self.compositions.push(composition);
        self
    }

    /// Retrieve the default source for Images unless explicitly specified per Image.
    pub fn source(&self) -> &Source {
        &self.default_source
    }

    /// Execute the test with the constructed environment in full operation.
    ///
    /// # Synchronous
    /// This non-async version creates its own runtime to execute the test.
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

        let equal = matches!(*test.source(), Source::Local);

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

        let equal = matches!(test.default_source, Source::DockerHub);

        assert!(equal, "default_source was not set correctly");
    }
}
