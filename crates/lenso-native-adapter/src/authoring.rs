use std::{
    cell::{Cell, OnceCell},
    fmt,
    rc::Rc,
    time::Duration,
};

use lenso_kernel::{
    ActivateContext, CancellationToken, DeactivateContext, PluginDependencies, PluginFuture,
    PluginLifecycle, RuntimeFailure,
};

/// Bounded cooperative context available to complete-object construction and cleanup.
#[derive(Clone, Debug)]
pub struct LifecycleContext {
    cancellation: CancellationToken,
    remaining_budget: Option<Duration>,
}

impl LifecycleContext {
    fn constructing(context: &ActivateContext) -> Self {
        Self {
            cancellation: context.cancellation(),
            remaining_budget: None,
        }
    }

    fn stopping(context: &DeactivateContext) -> Self {
        Self {
            cancellation: context.cancellation(),
            remaining_budget: context.remaining_budget(),
        }
    }

    /// Returns the cooperative cancellation signal for this lifecycle phase.
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Returns the Host cleanup budget remaining when one is configured.
    pub const fn remaining_budget(&self) -> Option<Duration> {
        self.remaining_budget
    }
}

/// Exact inputs admitted before one complete Plugin object is constructed.
#[derive(Clone, Debug)]
pub struct ConstructionContext {
    configuration: String,
    dependencies: PluginDependencies,
    lifecycle: LifecycleContext,
}

impl ConstructionContext {
    fn new(configuration: String, context: &ActivateContext) -> Self {
        Self {
            configuration,
            dependencies: context.dependencies().clone(),
            lifecycle: LifecycleContext::constructing(context),
        }
    }

    /// Returns validated, defaulted Plugin configuration JSON.
    pub fn configuration(&self) -> &str {
        &self.configuration
    }

    /// Returns only the dependencies declared for this Plugin instance.
    pub const fn dependencies(&self) -> &PluginDependencies {
        &self.dependencies
    }

    /// Returns the construction cancellation context.
    pub const fn lifecycle(&self) -> &LifecycleContext {
        &self.lifecycle
    }
}

/// Future returned by one generated complete-object constructor.
pub type ConstructionFuture<T> =
    futures::future::LocalBoxFuture<'static, Result<T, RuntimeFailure>>;

type Constructor<T> = Rc<dyn Fn(ConstructionContext) -> ConstructionFuture<T>>;
type Stopper<T> = Rc<dyn Fn(Rc<T>, LifecycleContext) -> PluginFuture>;

/// Shared reference to the one object owned by a Plugin instance generation.
pub struct PluginObject<T> {
    value: Rc<OnceCell<Rc<T>>>,
}

impl<T> PluginObject<T> {
    /// Creates an inert provider handle before construction begins.
    pub fn empty() -> Self {
        Self {
            value: Rc::new(OnceCell::new()),
        }
    }

    fn install(&self, value: T) -> Result<(), RuntimeFailure> {
        self.value
            .set(Rc::new(value))
            .map_err(|_| RuntimeFailure::InvalidResolvedPlan {
                detail: "Plugin object was constructed more than once".to_owned(),
            })
    }

    /// Returns the constructed object or fails closed before readiness.
    pub fn get(&self) -> Result<Rc<T>, RuntimeFailure> {
        self.value
            .get()
            .cloned()
            .ok_or(RuntimeFailure::AdmissionClosed)
    }
}

impl<T> Clone for PluginObject<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
        }
    }
}

impl<T> fmt::Debug for PluginObject<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginObject")
            .field("constructed", &self.value.get().is_some())
            .finish()
    }
}

/// Adapter lifecycle that owns exactly one complete Plugin object.
pub struct CompleteObjectLifecycle<T> {
    object: PluginObject<T>,
    configuration: String,
    constructor: Constructor<T>,
    stopper: Option<Stopper<T>>,
    construction_started: Cell<bool>,
    stop_attempted: Cell<bool>,
}

impl<T> CompleteObjectLifecycle<T> {
    /// Creates a lifecycle around generated constructor and optional stop glue.
    pub fn new(
        object: PluginObject<T>,
        configuration: impl Into<String>,
        constructor: impl Fn(ConstructionContext) -> ConstructionFuture<T> + 'static,
    ) -> Self {
        Self {
            object,
            configuration: configuration.into(),
            constructor: Rc::new(constructor),
            stopper: None,
            construction_started: Cell::new(false),
            stop_attempted: Cell::new(false),
        }
    }

    /// Adds the generated stop hook for the same complete object.
    #[must_use]
    pub fn with_stop(
        mut self,
        stopper: impl Fn(Rc<T>, LifecycleContext) -> PluginFuture + 'static,
    ) -> Self {
        self.stopper = Some(Rc::new(stopper));
        self
    }
}

impl<T> fmt::Debug for CompleteObjectLifecycle<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompleteObjectLifecycle")
            .field("object", &self.object)
            .field("has_stop", &self.stopper.is_some())
            .finish_non_exhaustive()
    }
}

impl<T: 'static> PluginLifecycle for CompleteObjectLifecycle<T> {
    fn construct(&self, context: ActivateContext) -> PluginFuture {
        if self.construction_started.replace(true) {
            return Box::pin(futures::future::ready(Err(
                RuntimeFailure::InvalidResolvedPlan {
                    detail: "Plugin construction was attempted more than once".to_owned(),
                },
            )));
        }
        let object = self.object.clone();
        let construction = (self.constructor)(ConstructionContext::new(
            self.configuration.clone(),
            &context,
        ));
        let cancellation = context.cancellation();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(RuntimeFailure::AdmissionClosed);
            }
            let value = construction.await?;
            object.install(value)?;
            if cancellation.is_cancelled() {
                return Err(RuntimeFailure::AdmissionClosed);
            }
            Ok(())
        })
    }

    fn deactivate(&self, context: DeactivateContext) -> PluginFuture {
        if self.stop_attempted.replace(true) {
            return Box::pin(futures::future::ready(Ok(())));
        }
        let Some(stopper) = self.stopper.clone() else {
            return Box::pin(futures::future::ready(Ok(())));
        };
        let Ok(object) = self.object.get() else {
            return Box::pin(futures::future::ready(Ok(())));
        };
        stopper(object, LifecycleContext::stopping(&context))
    }
}
