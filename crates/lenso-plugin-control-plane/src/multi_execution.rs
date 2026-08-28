use std::{collections::BTreeSet, rc::Rc};

use lenso_dylib_adapter::{DylibAdapter, DylibLimits, DylibVerifier};
use lenso_kernel::ExecutionAdapterCatalog;
use lenso_process_adapter::{ProcessAdapter, ProcessLimits};
use lenso_quickjs_adapter::{QuickJsAdapter, QuickJsLimits};
use lenso_remote_adapter::{RemoteAdapter, RemoteLimits};
use lenso_runtime_codec::JsonCapabilityCodec;
use lenso_wasm_component_adapter::{WasmComponentAdapter, WasmComponentLimits};

use crate::{CatalogFactory, ControlPlaneError, ResolvedGeneration};

/// Product Host Build factory that assembles supported dynamic execution classes.
pub struct MultiExecutionCatalogFactory<B: CatalogFactory> {
    base: B,
    wasm_codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
    quickjs_codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
    process_codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
    remote_codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
    remote_http_client: Option<reqwest::blocking::Client>,
    dylib_codecs: Vec<Rc<dyn JsonCapabilityCodec>>,
    dylib_verifier: Option<Rc<dyn DylibVerifier>>,
    wasm_limits: WasmComponentLimits,
    quickjs_limits: QuickJsLimits,
    process_limits: ProcessLimits,
    remote_limits: RemoteLimits,
    dylib_limits: DylibLimits,
}

impl<B: CatalogFactory> MultiExecutionCatalogFactory<B> {
    /// Wraps the product's existing native/process catalog factory.
    pub fn new(base: B) -> Self {
        Self {
            base,
            wasm_codecs: Vec::new(),
            quickjs_codecs: Vec::new(),
            process_codecs: Vec::new(),
            remote_codecs: Vec::new(),
            remote_http_client: None,
            dylib_codecs: Vec::new(),
            dylib_verifier: None,
            wasm_limits: WasmComponentLimits::default(),
            quickjs_limits: QuickJsLimits::default(),
            process_limits: ProcessLimits::default(),
            remote_limits: RemoteLimits::default(),
            dylib_limits: DylibLimits::default(),
        }
    }

    /// Registers a generated codec for Wasm Component Instances.
    #[must_use]
    pub fn with_wasm_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.wasm_codecs.push(Rc::new(codec));
        self
    }

    /// Registers a generated codec for `QuickJS` Instances.
    #[must_use]
    pub fn with_quickjs_codec(mut self, codec: impl JsonCapabilityCodec) -> Self {
        self.quickjs_codecs.push(Rc::new(codec));
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

    /// Applies product-owned limits for all three execution classes.
    #[must_use]
    pub fn with_limits(
        mut self,
        wasm: WasmComponentLimits,
        quickjs: QuickJsLimits,
        dylib: DylibLimits,
    ) -> Self {
        self.wasm_limits = wasm;
        self.quickjs_limits = quickjs;
        self.dylib_limits = dylib;
        self
    }
}

impl<B: CatalogFactory> std::fmt::Debug for MultiExecutionCatalogFactory<B> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MultiExecutionCatalogFactory")
            .field("wasm_codecs", &self.wasm_codecs.len())
            .field("quickjs_codecs", &self.quickjs_codecs.len())
            .field("process_codecs", &self.process_codecs.len())
            .field("remote_codecs", &self.remote_codecs.len())
            .field("has_remote_http_client", &self.remote_http_client.is_some())
            .field("dylib_codecs", &self.dylib_codecs.len())
            .field("has_dylib_verifier", &self.dylib_verifier.is_some())
            .finish_non_exhaustive()
    }
}

impl<B: CatalogFactory> CatalogFactory for MultiExecutionCatalogFactory<B> {
    fn catalog(
        &self,
        generation: &ResolvedGeneration,
    ) -> Result<ExecutionAdapterCatalog, ControlPlaneError> {
        let selected: BTreeSet<_> = generation
            .plan
            .plugin_instances()
            .iter()
            .map(|instance| instance.execution_class().as_str())
            .collect();
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
        let available = catalog.execution_classes();
        for execution_class in selected {
            if !available
                .iter()
                .any(|available| available.as_str() == execution_class)
            {
                return Err(ControlPlaneError::HostFailure {
                    detail: format!(
                        "Host Build has no installed Adapter for selected class `{execution_class}`"
                    ),
                });
            }
        }
        Ok(catalog)
    }
}

fn catalog_error(error: impl std::fmt::Display) -> ControlPlaneError {
    ControlPlaneError::HostFailure {
        detail: error.to_string(),
    }
}
