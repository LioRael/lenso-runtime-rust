use std::{collections::BTreeSet, rc::Rc};

use lenso_dylib_adapter::{DylibAdapter, DylibLimits, DylibVerifier};
use lenso_kernel::ExecutionAdapterCatalog;
use lenso_process_adapter::{ProcessAdapter, ProcessLimits};
use lenso_quickjs_adapter::{QuickJsAdapter, QuickJsLimits};
use lenso_remote_adapter::{RemoteAdapter, RemoteLimits};
use lenso_runtime_codec::JsonCapabilityCodec;
use lenso_wasm_component_adapter::{WasmComponentAdapter, WasmComponentLimits};

use crate::{CatalogFactory, ControlPlaneError, ResolvedGeneration};

/// Catalog factory for native/base, Process, `QuickJS`, and Wasm execution classes.
///
/// This narrows catalog composition only. The control-plane crate still depends on
/// every Adapter in its build closure.
pub struct CoreExecutionCatalogFactory<B: CatalogFactory> {
    base: B,
    wasm_codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
    quickjs_codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
    process_codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
    deferred_execution_classes: BTreeSet<String>,
    wasm_limits: WasmComponentLimits,
    quickjs_limits: QuickJsLimits,
    process_limits: ProcessLimits,
}

impl<B: CatalogFactory> CoreExecutionCatalogFactory<B> {
    /// Wraps the product's existing native/base catalog factory.
    pub fn new(base: B) -> Self {
        Self {
            base,
            wasm_codecs: Vec::new(),
            quickjs_codecs: Vec::new(),
            process_codecs: Vec::new(),
            deferred_execution_classes: BTreeSet::new(),
            wasm_limits: WasmComponentLimits::default(),
            quickjs_limits: QuickJsLimits::default(),
            process_limits: ProcessLimits::default(),
        }
    }

    /// Registers a generated codec for Wasm Component Instances.
    #[must_use]
    pub fn with_wasm_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.wasm_codecs.push(Rc::new(codec));
        self
    }

    /// Applies product-owned limits for Wasm Component Instances.
    #[must_use]
    pub fn with_wasm_limits(mut self, limits: WasmComponentLimits) -> Self {
        self.wasm_limits = limits;
        self
    }

    /// Registers a generated codec for `QuickJS` Instances.
    #[must_use]
    pub fn with_quickjs_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.quickjs_codecs.push(Rc::new(codec));
        self
    }

    /// Applies product-owned limits for `QuickJS` Instances.
    #[must_use]
    pub fn with_quickjs_limits(mut self, limits: QuickJsLimits) -> Self {
        self.quickjs_limits = limits;
        self
    }

    /// Registers a generated codec for trusted Process Instances.
    #[must_use]
    pub fn with_process_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.process_codecs.push(Rc::new(codec));
        self
    }

    /// Applies product-owned limits for trusted Process Instances.
    #[must_use]
    pub fn with_process_limits(mut self, limits: ProcessLimits) -> Self {
        self.process_limits = limits;
        self
    }

    /// Defers one product-specific execution class to an outer Catalog factory.
    #[must_use]
    pub fn with_deferred_execution_class(mut self, execution_class: impl Into<String>) -> Self {
        self.deferred_execution_classes
            .insert(execution_class.into());
        self
    }

    fn assemble(
        &self,
        generation: &ResolvedGeneration,
        selected: &BTreeSet<&str>,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        let mut catalog = self.base.catalog(generation)?;
        if selected.contains(lenso_wasm_component_adapter::EXECUTION_CLASS) {
            let adapter = self.wasm_codecs.iter().cloned().fold(
                WasmComponentAdapter::new(generation.artifacts.clone())
                    .with_limits(self.wasm_limits.clone()),
                WasmComponentAdapter::with_shared_codec,
            );
            catalog = catalog.with_adapter(adapter).map_err(catalog_error)?;
        }
        if selected.contains(lenso_quickjs_adapter::EXECUTION_CLASS) {
            let adapter = self.quickjs_codecs.iter().cloned().fold(
                QuickJsAdapter::new(generation.artifacts.clone())
                    .with_limits(self.quickjs_limits.clone()),
                QuickJsAdapter::with_shared_codec,
            );
            catalog = catalog.with_adapter(adapter).map_err(catalog_error)?;
        }
        if selected.contains(lenso_process_adapter::EXECUTION_CLASS) {
            let adapter = self.process_codecs.iter().cloned().fold(
                ProcessAdapter::new(generation.artifacts.clone())
                    .with_limits(self.process_limits.clone()),
                ProcessAdapter::with_shared_codec,
            );
            catalog = catalog.with_adapter(adapter).map_err(catalog_error)?;
        }
        Ok(catalog)
    }

    fn validate(
        &self,
        catalog: &ExecutionAdapterCatalog,
        selected: &BTreeSet<&str>,
    ) -> Result<(), ControlPlaneError> {
        let available = catalog.execution_classes();
        for execution_class in selected {
            if !available
                .iter()
                .any(|item| item.as_str() == *execution_class)
                && !self.deferred_execution_classes.contains(*execution_class)
            {
                return Err(ControlPlaneError::HostFailure {
                    detail: format!(
                        "Host Build has no installed Adapter for selected class `{execution_class}`"
                    ),
                });
            }
        }
        Ok(())
    }
}

impl<B: CatalogFactory> std::fmt::Debug for CoreExecutionCatalogFactory<B> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoreExecutionCatalogFactory")
            .field("wasm_codecs", &self.wasm_codecs.len())
            .field("quickjs_codecs", &self.quickjs_codecs.len())
            .field("process_codecs", &self.process_codecs.len())
            .field(
                "deferred_execution_classes",
                &self.deferred_execution_classes,
            )
            .finish_non_exhaustive()
    }
}

impl<B: CatalogFactory> CatalogFactory for CoreExecutionCatalogFactory<B> {
    fn catalog(
        &self,
        generation: &ResolvedGeneration,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        let selected = selected_execution_classes(generation);
        let catalog = self.assemble(generation, &selected)?;
        self.validate(&catalog, &selected)?;
        Ok(catalog)
    }
}

/// Product Host Build factory that assembles supported dynamic execution classes.
pub struct MultiExecutionCatalogFactory<B: CatalogFactory> {
    core: CoreExecutionCatalogFactory<B>,
    remote_codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
    remote_http_client: Option<reqwest::blocking::Client>,
    dylib_codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
    dylib_verifier: Option<Rc<dyn DylibVerifier>>,
    remote_limits: RemoteLimits,
    dylib_limits: DylibLimits,
}

impl<B: CatalogFactory> MultiExecutionCatalogFactory<B> {
    /// Wraps the product's existing native/process catalog factory.
    pub fn new(base: B) -> Self {
        Self {
            core: CoreExecutionCatalogFactory::new(base),
            remote_codecs: Vec::new(),
            remote_http_client: None,
            dylib_codecs: Vec::new(),
            dylib_verifier: None,
            remote_limits: RemoteLimits::default(),
            dylib_limits: DylibLimits::default(),
        }
    }

    /// Registers a generated codec for Wasm Component Instances.
    #[must_use]
    pub fn with_wasm_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.core = self.core.with_wasm_codec(codec);
        self
    }

    /// Registers a generated codec for `QuickJS` Instances.
    #[must_use]
    pub fn with_quickjs_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.core = self.core.with_quickjs_codec(codec);
        self
    }

    /// Registers a generated codec for trusted Process Instances.
    #[must_use]
    pub fn with_process_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.core = self.core.with_process_codec(codec);
        self
    }

    /// Applies product-owned limits for trusted Process Instances.
    #[must_use]
    pub fn with_process_limits(mut self, limits: ProcessLimits) -> Self {
        self.core = self.core.with_process_limits(limits);
        self
    }

    /// Registers a generated codec for remote HTTP Instances.
    #[must_use]
    pub fn with_remote_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.remote_codecs.push(Rc::new(codec));
        self
    }

    /// Applies product-owned limits for remote HTTP Instances.
    #[must_use]
    pub fn with_remote_limits(mut self, limits: RemoteLimits) -> Self {
        self.remote_limits = limits;
        self
    }

    /// Installs product-owned proxy, identity, or mTLS policy for Remote Instances.
    #[must_use]
    pub fn with_remote_http_client(mut self, client: reqwest::blocking::Client) -> Self {
        self.remote_http_client = Some(client);
        self
    }

    /// Registers a generated codec for trusted native dylib Instances.
    #[must_use]
    pub fn with_dylib_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.dylib_codecs.push(Rc::new(codec));
        self
    }

    /// Installs the exact host trust verifier required before dylib loading.
    #[must_use]
    pub fn with_dylib_verifier(mut self, verifier: impl DylibVerifier) -> Self {
        self.dylib_verifier = Some(Rc::new(verifier));
        self
    }

    /// Defers one product-specific execution class to an outer Catalog factory.
    #[must_use]
    pub fn with_deferred_execution_class(mut self, execution_class: impl Into<String>) -> Self {
        self.core = self.core.with_deferred_execution_class(execution_class);
        self
    }

    /// Applies product-owned limits for all three execution classes.
    #[must_use]
    pub fn with_limits(
        mut self,
        wasm: WasmComponentLimits,
        quickjs: QuickJsLimits,
        dylib: DylibLimits,
    ) -> Self {
        self.core = self
            .core
            .with_wasm_limits(wasm)
            .with_quickjs_limits(quickjs);
        self.dylib_limits = dylib;
        self
    }
}

impl<B: CatalogFactory> std::fmt::Debug for MultiExecutionCatalogFactory<B> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MultiExecutionCatalogFactory")
            .field("wasm_codecs", &self.core.wasm_codecs.len())
            .field("quickjs_codecs", &self.core.quickjs_codecs.len())
            .field("process_codecs", &self.core.process_codecs.len())
            .field("remote_codecs", &self.remote_codecs.len())
            .field("has_remote_http_client", &self.remote_http_client.is_some())
            .field("dylib_codecs", &self.dylib_codecs.len())
            .field("has_dylib_verifier", &self.dylib_verifier.is_some())
            .field(
                "deferred_execution_classes",
                &self.core.deferred_execution_classes,
            )
            .finish_non_exhaustive()
    }
}

impl<B: CatalogFactory> CatalogFactory for MultiExecutionCatalogFactory<B> {
    fn catalog(
        &self,
        generation: &ResolvedGeneration,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        let selected = selected_execution_classes(generation);
        let mut catalog = self.core.assemble(generation, &selected)?;
        if selected.contains(lenso_remote_adapter::EXECUTION_CLASS) {
            let mut adapter = self.remote_codecs.iter().cloned().fold(
                RemoteAdapter::new(generation.artifacts.clone())
                    .with_limits(self.remote_limits.clone()),
                RemoteAdapter::with_shared_codec,
            );
            if let Some(client) = &self.remote_http_client {
                adapter = adapter.with_http_client(client.clone());
            }
            catalog = catalog.with_adapter(adapter).map_err(catalog_error)?;
        }
        if selected.contains(lenso_dylib_adapter::EXECUTION_CLASS) {
            let verifier =
                self.dylib_verifier
                    .clone()
                    .ok_or_else(|| ControlPlaneError::HostFailure {
                        detail: "Host Build selected native dylib without a trust verifier"
                            .to_owned(),
                    })?;
            let adapter = self.dylib_codecs.iter().cloned().fold(
                DylibAdapter::with_shared_verifier(generation.artifacts.clone(), verifier)
                    .with_limits(self.dylib_limits.clone()),
                DylibAdapter::with_shared_codec,
            );
            catalog = catalog.with_adapter(adapter).map_err(catalog_error)?;
        }
        self.core.validate(&catalog, &selected)?;
        Ok(catalog)
    }
}

fn selected_execution_classes(generation: &ResolvedGeneration) -> BTreeSet<&str> {
    generation
        .plan
        .plugin_instances()
        .iter()
        .map(|instance| instance.execution_class().as_str())
        .collect()
}

fn catalog_error(error: impl std::fmt::Display) -> ControlPlaneError {
    ControlPlaneError::HostFailure {
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use lenso_app_plan::{ExecutionClassId, PluginInstancePlan, ResolvedAppPlan};
    use lenso_runtime_codec::{ArtifactCatalog, InstanceResourceCatalog};

    use super::*;
    use crate::{AppGenerationSpec, CanonicalDocument, EffectiveHostGrantSet, ResolvedArtifactSet};

    #[derive(Debug)]
    struct EmptyFactory;

    impl CatalogFactory for EmptyFactory {
        fn catalog(
            &self,
            _generation: &ResolvedGeneration,
        ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
            Ok(ExecutionAdapterCatalog::new())
        }
    }

    #[test]
    fn core_factory_can_defer_an_external_execution_class() {
        CoreExecutionCatalogFactory::new(EmptyFactory)
            .with_deferred_execution_class("example.external@1")
            .catalog(&generation_selecting("example.external@1"))
            .expect("the outer factory supplies the deferred Adapter");
    }

    #[test]
    fn core_factory_fails_closed_for_unavailable_external_adapters() {
        for execution_class in [
            lenso_remote_adapter::EXECUTION_CLASS,
            lenso_dylib_adapter::EXECUTION_CLASS,
        ] {
            let error = CoreExecutionCatalogFactory::new(EmptyFactory)
                .catalog(&generation_selecting(execution_class))
                .expect_err("an unavailable Adapter must fail closed");
            assert_eq!(
                error.to_string(),
                format!(
                    "Host Build has no installed Adapter for selected class `{execution_class}`"
                )
            );
        }
    }

    #[test]
    fn full_factory_can_defer_an_external_execution_class() {
        MultiExecutionCatalogFactory::new(EmptyFactory)
            .with_deferred_execution_class("example.external@1")
            .catalog(&generation_selecting("example.external@1"))
            .expect("the outer factory supplies the deferred Adapter");
    }

    #[test]
    fn full_factory_requires_a_dylib_trust_verifier() {
        let error = MultiExecutionCatalogFactory::new(EmptyFactory)
            .catalog(&generation_selecting(lenso_dylib_adapter::EXECUTION_CLASS))
            .expect_err("native dylib selection must require explicit trust");
        assert_eq!(
            error.to_string(),
            "Host Build selected native dylib without a trust verifier"
        );
    }

    fn generation_selecting(execution_class: &str) -> ResolvedGeneration {
        let artifact_set = CanonicalDocument::from_value(
            "artifacts",
            ResolvedArtifactSet {
                schema_version: 3,
                resolution_authority_digest: "authority".to_owned(),
                host_execution_policy_digest: "policy".to_owned(),
                artifacts: Vec::new(),
                instance_resources: Vec::new(),
            },
        )
        .unwrap();
        let grants = CanonicalDocument::from_value(
            "grants",
            EffectiveHostGrantSet {
                schema_version: 2,
                resolution_authority_digest: "authority".to_owned(),
                grants: Vec::new(),
            },
        )
        .unwrap();
        let spec = CanonicalDocument::from_value(
            "generation",
            AppGenerationSpec {
                schema_version: 2,
                app_id: "example.app".to_owned(),
                host_build_manifest_digest: "host".to_owned(),
                host_execution_policy_digest: "policy".to_owned(),
                resolved_plan_digest: "plan".to_owned(),
                resolution_authority_digest: "authority".to_owned(),
                resolved_artifact_set_digest: artifact_set.digest().to_owned(),
                effective_host_grant_set_digest: grants.digest().to_owned(),
            },
        )
        .unwrap();
        ResolvedGeneration {
            plan: ResolvedAppPlan::new(
                vec![
                    PluginInstancePlan::new("selected", "test.selected")
                        .with_execution_class(ExecutionClassId::new(execution_class)),
                ],
                Vec::new(),
            ),
            artifact_set,
            grants,
            spec,
            artifacts: ArtifactCatalog::new(),
            resources: InstanceResourceCatalog::new(),
            stateful_instances: BTreeMap::new(),
        }
    }
}
