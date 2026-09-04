use std::{
    any::{Any, TypeId},
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
    futures::future::LocalBoxFuture<'static, Result<Rc<T>, RuntimeFailure>>;

/// Type-erased future contributed by generated constructor glue.
pub type ErasedConstructionFuture =
    futures::future::LocalBoxFuture<'static, Result<Rc<dyn Any>, RuntimeFailure>>;
/// Type-erased stop hook contributed by generated Plugin glue.
pub type ErasedStop = fn(Rc<dyn Any>, LifecycleContext) -> PluginFuture;

/// One link-time constructor and stop pair for a complete-object Plugin type.
#[derive(Clone, Copy)]
pub struct LinkedPluginConstruction {
    plugin_type: fn() -> TypeId,
    custom: bool,
    construct: fn(ConstructionContext) -> ErasedConstructionFuture,
    stop: Option<ErasedStop>,
}

impl LinkedPluginConstruction {
    /// Creates generated link-time construction metadata.
    #[doc(hidden)]
    pub const fn new(
        plugin_type: fn() -> TypeId,
        custom: bool,
        construct: fn(ConstructionContext) -> ErasedConstructionFuture,
        stop: Option<ErasedStop>,
    ) -> Self {
        Self {
            plugin_type,
            custom,
            construct,
            stop,
        }
    }
}

impl fmt::Debug for LinkedPluginConstruction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinkedPluginConstruction")
            .field("custom", &self.custom)
            .finish_non_exhaustive()
    }
}

inventory::collect!(LinkedPluginConstruction);

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

    /// Creates a provider handle around an already constructed legacy object.
    pub fn from_value(value: Rc<T>) -> Self {
        let object = Self::empty();
        assert!(
            object.value.set(value).is_ok(),
            "a new Plugin object cell is empty"
        );
        object
    }

    fn install(&self, value: Rc<T>) -> Result<(), RuntimeFailure> {
        self.value
            .set(value)
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

    /// Selects the unique generated constructor for this exact Plugin type.
    pub fn linked(
        object: PluginObject<T>,
        configuration: impl Into<String>,
    ) -> Result<Self, RuntimeFailure>
    where
        T: Any,
    {
        let mut defaults = Vec::new();
        let mut customs = Vec::new();
        for linked in inventory::iter::<LinkedPluginConstruction> {
            if (linked.plugin_type)() == TypeId::of::<T>() {
                if linked.custom {
                    customs.push(linked);
                } else {
                    defaults.push(linked);
                }
            }
        }
        let selected = match (customs.as_slice(), defaults.as_slice()) {
            ([custom], _) => *custom,
            ([], [default]) => *default,
            ([], []) => {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: "Plugin type has no linked constructor".to_owned(),
                });
            }
            (customs, _) if customs.len() > 1 => {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: "Plugin type has multiple custom constructors".to_owned(),
                });
            }
            _ => {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: "Plugin type has multiple default constructors".to_owned(),
                });
            }
        };
        let constructor = selected.construct;
        let lifecycle = Self::new(object, configuration, move |context| {
            let erased = constructor(context);
            Box::pin(async move {
                erased
                    .await?
                    .downcast::<T>()
                    .map_err(|_| RuntimeFailure::InvalidResolvedPlan {
                        detail: "linked Plugin constructor returned the wrong type".to_owned(),
                    })
            })
        });
        Ok(if let Some(stop) = selected.stop {
            lifecycle.with_stop(move |object, context| {
                let object: Rc<dyn Any> = object;
                stop(object, context)
            })
        } else {
            lifecycle
        })
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
