//! A small deterministic App harness for testing real native Plugin composition.
//!
//! `TestApp` keeps the same immutable Plan and native Adapter path as production,
//! while removing Tokio setup and manual Driver plumbing from Plugin tests.

use std::{future::Future, time::Duration};

use lenso_app_plan::ResolvedAppPlan;
use lenso_kernel::{
    DeterministicDriver, Kernel, NativeApp, PluginDependencies, RuntimeFailure, ShutdownOutcome,
};
use lenso_native_adapter::{NativePluginFactory, NativePluginRegistry};
use lenso_plugin_authoring::CapabilityClient;

/// Builder for one deterministic native Test App.
#[derive(Debug)]
pub struct TestAppBuilder {
    plan: ResolvedAppPlan,
    registry: NativePluginRegistry,
}

impl TestAppBuilder {
    /// Starts with an exact immutable Plan.
    pub fn new(plan: ResolvedAppPlan) -> Self {
        Self {
            plan,
            registry: NativePluginRegistry::new(),
        }
    }

    /// Adds a native factory available to this test Host.
    #[must_use]
    pub fn with_factory(mut self, factory: impl NativePluginFactory) -> Self {
        self.registry = self.registry.with_factory(factory);
        self
    }

    /// Adds factories linked into the test binary.
    #[must_use]
    pub fn with_linked_factories(mut self) -> Self {
        self.registry = self.registry.with_linked_factories();
        self
    }

    /// Boots the exact Plan through Kernel and the native Adapter.
    pub fn start(self) -> Result<TestApp, RuntimeFailure> {
        let driver = DeterministicDriver::new();
        let app = driver.run(Kernel::start_native(
            self.plan,
            driver.clone(),
            self.registry,
        ))?;
        Ok(TestApp { driver, app })
    }
}

/// A running deterministic App that exposes the real Kernel handle to tests.
#[derive(Debug)]
pub struct TestApp {
    driver: DeterministicDriver,
    app: NativeApp,
}

impl TestApp {
    /// Creates a builder for an exact immutable Plan.
    pub fn builder(plan: ResolvedAppPlan) -> TestAppBuilder {
        TestAppBuilder::new(plan)
    }

    /// Returns the running App for typed generated Client handles and diagnostics.
    pub fn app(&self) -> &NativeApp {
        &self.app
    }

    /// Connects one generated Client from a consumer Instance's Plan-owned bindings.
    pub fn client<C>(&self, consumer_instance: &str) -> Result<C, RuntimeFailure>
    where
        C: CapabilityClient<Dependencies = PluginDependencies, Error = RuntimeFailure>,
    {
        let dependencies = self.app.dependencies(consumer_instance)?;
        C::from_dependencies(&dependencies)
    }

    /// Runs one future to completion on deterministic virtual time.
    pub fn run<F: Future>(&self, future: F) -> F::Output {
        self.driver.run(future)
    }

    /// Advances deterministic virtual time and wakes elapsed timers.
    pub fn advance(&self, duration: Duration) {
        self.driver.advance(duration);
    }

    /// Shuts down the App and returns the exact cleanup outcome.
    pub fn shutdown(&self, timeout: Duration) -> ShutdownOutcome {
        self.driver.run(self.app.shutdown(timeout))
    }
}

#[cfg(test)]
mod tests {
    use lenso_app_plan::PluginInstancePlan;

    use super::*;

    #[test]
    fn empty_test_app_starts_ready_and_shuts_down_cleanly() {
        let app = TestApp::builder(ResolvedAppPlan::empty()).start().unwrap();

        assert!(app.app().is_ready());
        assert_eq!(app.shutdown(Duration::from_secs(1)), ShutdownOutcome::Clean);
    }

    #[test]
    fn plan_selected_plugin_without_a_factory_fails_closed() {
        let plan = ResolvedAppPlan::new(
            vec![PluginInstancePlan::new("missing", "test.missing")],
            vec![],
        );

        let error = TestApp::builder(plan).start().unwrap_err();
        assert!(
            matches!(error, RuntimeFailure::MissingPluginFactory { instance, .. } if instance == "missing")
        );
    }
}
