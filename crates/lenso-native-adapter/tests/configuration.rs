use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use lenso_app_plan::{PluginInstancePlan, ResolvedAppPlan, RestartPolicy};
use lenso_kernel::{DeterministicDriver, Kernel, RuntimeDriver, RuntimeFailure, ShutdownOutcome};
use lenso_native_adapter::{
    CompleteObjectLifecycle, InstanceResources, LifecycleContext, NativePluginFactory,
    NativePluginFactoryContext, NativePluginInstance, NativePluginRegistry, PluginObject,
};
use lenso_runtime_codec::InstanceResourceCatalog;

#[derive(Debug)]
struct RecordingFactory {
    observed: Rc<RefCell<Vec<(String, String, String)>>>,
}

impl NativePluginFactory for RecordingFactory {
    fn package_id(&self) -> &'static str {
        "test.configured"
    }
    fn package_version(&self) -> &'static str {
        "1.0.0"
    }

    fn instantiate(
        &self,
        context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        self.observed.borrow_mut().push((
            context.instance_key().to_owned(),
            context.entrypoint().to_owned(),
            context.configuration().to_owned(),
        ));
        Ok(NativePluginInstance::default())
    }
}

#[derive(Debug)]
struct PluginFactory {
    observed: Rc<RefCell<Vec<(String, String, String)>>>,
}

#[derive(Debug)]
struct ResourceFactory {
    observed: Rc<RefCell<Vec<String>>>,
}

impl NativePluginFactory for ResourceFactory {
    fn package_id(&self) -> &'static str {
        "test.resources"
    }

    fn package_version(&self) -> &'static str {
        "1.0.0"
    }

    fn instantiate(
        &self,
        context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        self.observed.borrow_mut().push(
            context
                .resources()
                .read_text("prompts/system.md")?
                .to_owned(),
        );
        Ok(NativePluginInstance::default())
    }
}

impl NativePluginFactory for PluginFactory {
    fn package_id(&self) -> &'static str {
        "test.configured"
    }

    fn package_version(&self) -> &'static str {
        "1.0.0"
    }

    fn factory_identity(&self) -> String {
        "test.configured@host-build-a".to_owned()
    }

    fn instantiate(
        &self,
        context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        self.observed.borrow_mut().push((
            context.instance_key().to_owned(),
            context.entrypoint().to_owned(),
            context.configuration().to_owned(),
        ));
        Ok(NativePluginInstance::default())
    }
}

#[test]
fn native_factory_version_must_match_the_authoring_resolved_version() {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("configured", "test.configured").with_package_revision("2.0.0"),
        ],
        vec![],
    );
    let driver = DeterministicDriver::new();
    let error = driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new().with_factory(RecordingFactory { observed }),
        ))
        .expect_err("a differently linked Cargo package must be rejected");
    assert!(matches!(error, RuntimeFailure::MissingPluginFactory { .. }));
}

#[test]
fn native_factory_authoring_profile_is_not_the_plan_execution_profile() {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("configured", "test.configured")
                .with_package_revision("1.0.0")
                .with_authoring(1, "lenso.native-rust@1"),
        ],
        vec![],
    );
    let driver = DeterministicDriver::new();

    driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new().with_factory(RecordingFactory {
                observed: observed.clone(),
            }),
        ))
        .expect("the selected native Adapter owns the Plan execution profile");

    assert_eq!(observed.borrow().len(), 1);
}

#[test]
fn native_factory_identity_matches_a_plugin_resolved_revision() {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("configured", "test.configured")
                .with_package_revision("test.configured@host-build-a"),
        ],
        vec![],
    );
    let driver = DeterministicDriver::new();

    driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new().with_factory(PluginFactory {
                observed: observed.clone(),
            }),
        ))
        .expect("the Plugin-resolved factory identity should select the linked factory");

    assert_eq!(
        *observed.borrow(),
        vec![(
            "configured".to_owned(),
            "default".to_owned(),
            "{}".to_owned(),
        )]
    );
}

#[test]
fn legacy_native_rust_profile_selects_a_v1_authoring_factory() {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("configured", "test.configured")
                .with_authoring(1, "lenso.native-rust@1")
                .with_package_revision("1.0.0"),
        ],
        vec![],
    );
    let driver = DeterministicDriver::new();

    driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new().with_factory(RecordingFactory {
                observed: observed.clone(),
            }),
        ))
        .expect("the legacy native-rust profile should select a v1 authoring factory");

    assert_eq!(
        *observed.borrow(),
        vec![(
            "configured".to_owned(),
            "default".to_owned(),
            "{}".to_owned(),
        )]
    );
}

#[test]
fn native_factory_receives_the_exact_immutable_instance_input() {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("configured", "test.configured")
                .with_entrypoint("native.main")
                .with_configuration(r#"{"mode":"test"}"#)
                .with_restart_policy(RestartPolicy::on_failure(
                    1,
                    Duration::from_secs(30),
                    Duration::ZERO,
                    Duration::ZERO,
                    Duration::from_secs(1),
                )),
        ],
        vec![],
    );
    let driver = DeterministicDriver::new();

    let app = driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new().with_factory(RecordingFactory {
                observed: observed.clone(),
            }),
        ))
        .expect("the configured native App should start");
    app.report_plugin_failure("configured")
        .expect("the configured Instance should schedule recreation");
    driver.run(async {
        for _ in 0..6 {
            driver.yield_now().await;
        }
    });

    assert_eq!(
        *observed.borrow(),
        vec![
            (
                "configured".to_owned(),
                "native.main".to_owned(),
                r#"{"mode":"test"}"#.to_owned(),
            ),
            (
                "configured".to_owned(),
                "native.main".to_owned(),
                r#"{"mode":"test"}"#.to_owned(),
            ),
        ]
    );
}

#[test]
fn native_factory_receives_the_generation_resource_snapshot() {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let plan = ResolvedAppPlan::new(
        vec![PluginInstancePlan::new("resource-reader", "test.resources")],
        vec![],
    );
    let resources = InstanceResourceCatalog::new()
        .with_resources(
            "resource-reader",
            InstanceResources::from_files([(
                "prompts/system.md".to_owned(),
                b"generation one".to_vec(),
            )])
            .expect("resource snapshot should be valid"),
        )
        .expect("resource authority should be unique");
    let driver = DeterministicDriver::new();

    driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new()
                .with_resources(resources)
                .with_factory(ResourceFactory {
                    observed: observed.clone(),
                }),
        ))
        .expect("the native App should start with snapshotted resources");

    assert_eq!(*observed.borrow(), vec!["generation one".to_owned()]);
}

#[derive(Debug)]
struct NonClonePlugin {
    configuration: String,
}

#[derive(Debug)]
struct CompleteObjectFactory {
    object: PluginObject<NonClonePlugin>,
    constructed: Rc<RefCell<usize>>,
    stopped: Rc<RefCell<usize>>,
}

impl NativePluginFactory for CompleteObjectFactory {
    fn package_id(&self) -> &'static str {
        "test.complete-object"
    }

    fn runtime_profile(&self) -> &'static str {
        "lenso.native-authoring@2"
    }

    fn instantiate(
        &self,
        context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        let constructed = self.constructed.clone();
        let stopped = self.stopped.clone();
        let lifecycle = CompleteObjectLifecycle::new(
            self.object.clone(),
            context.configuration(),
            move |context| {
                *constructed.borrow_mut() += 1;
                Box::pin(async move {
                    Ok(Rc::new(NonClonePlugin {
                        configuration: context.configuration().to_owned(),
                    }))
                })
            },
        )
        .with_stop(move |object, lifecycle| {
            *stopped.borrow_mut() += 1;
            Box::pin(async move {
                assert_eq!(object.configuration, r#"{"mode":"v2"}"#);
                assert!(lifecycle.remaining_budget().is_some());
                Ok(())
            })
        });
        Ok(NativePluginInstance::with_lifecycle(Vec::new(), lifecycle))
    }
}

#[test]
fn complete_object_profile_constructs_once_without_clone_and_stops_once() {
    let object = PluginObject::empty();
    let constructed = Rc::new(RefCell::new(0));
    let stopped = Rc::new(RefCell::new(0));
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("complete", "test.complete-object")
                .with_authoring(2, "lenso.native-authoring@2")
                .with_configuration(r#"{"mode":"v2"}"#),
        ],
        vec![],
    );
    let driver = DeterministicDriver::new();
    let app = driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new().with_factory(CompleteObjectFactory {
                object: object.clone(),
                constructed: constructed.clone(),
                stopped: stopped.clone(),
            }),
        ))
        .expect("authoring v2 complete object should construct");

    let first = object
        .get()
        .expect("constructed object should be installed");
    let second = object.get().expect("all providers should share the object");
    assert!(Rc::ptr_eq(&first, &second));
    assert_eq!(first.configuration, r#"{"mode":"v2"}"#);
    assert_eq!(*constructed.borrow(), 1);

    assert_eq!(
        driver.run(app.shutdown(Duration::from_secs(1))),
        ShutdownOutcome::Clean
    );
    assert_eq!(*stopped.borrow(), 1);
}

#[derive(Clone, Copy, Debug)]
enum ConstructionExit {
    Fail,
    CancelAfterReturn,
}

#[derive(Debug)]
struct ConstructionExitFactory {
    object: PluginObject<NonClonePlugin>,
    stopped: Rc<RefCell<usize>>,
    exit: ConstructionExit,
}

impl NativePluginFactory for ConstructionExitFactory {
    fn package_id(&self) -> &'static str {
        "test.construction-exit"
    }

    fn runtime_profile(&self) -> &'static str {
        "lenso.native-authoring@2"
    }

    fn instantiate(
        &self,
        context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        let exit = self.exit;
        let stopped = self.stopped.clone();
        let lifecycle = CompleteObjectLifecycle::new(
            self.object.clone(),
            context.configuration(),
            move |context| {
                Box::pin(async move {
                    match exit {
                        ConstructionExit::Fail => Err(RuntimeFailure::PluginFailure {
                            detail: "constructor failed".to_owned(),
                        }),
                        ConstructionExit::CancelAfterReturn => {
                            context.lifecycle().cancellation().cancel();
                            Ok(Rc::new(NonClonePlugin {
                                configuration: context.configuration().to_owned(),
                            }))
                        }
                    }
                })
            },
        )
        .with_stop(move |_object, _lifecycle| {
            *stopped.borrow_mut() += 1;
            Box::pin(futures::future::ready(Ok(())))
        });
        Ok(NativePluginInstance::with_lifecycle(Vec::new(), lifecycle))
    }
}

fn construction_exit_plan() -> ResolvedAppPlan {
    ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("exit", "test.construction-exit")
                .with_authoring(2, "lenso.native-authoring@2")
                .with_configuration(r#"{"mode":"exit"}"#),
        ],
        vec![],
    )
}

#[test]
fn failed_construction_does_not_stop_an_object_that_never_existed() {
    let object = PluginObject::empty();
    let stopped = Rc::new(RefCell::new(0));
    let driver = DeterministicDriver::new();
    let error = driver
        .run(Kernel::start_native(
            construction_exit_plan(),
            driver.clone(),
            NativePluginRegistry::new().with_factory(ConstructionExitFactory {
                object: object.clone(),
                stopped: stopped.clone(),
                exit: ConstructionExit::Fail,
            }),
        ))
        .expect_err("failed construction must fail startup");

    assert!(matches!(error, RuntimeFailure::PluginFailure { .. }));
    assert!(object.get().is_err());
    assert_eq!(*stopped.borrow(), 0);
}

#[test]
fn late_constructor_success_after_cancellation_is_owned_and_stopped_once() {
    let object = PluginObject::empty();
    let stopped = Rc::new(RefCell::new(0));
    let driver = DeterministicDriver::new();
    let error = driver
        .run(Kernel::start_native(
            construction_exit_plan(),
            driver.clone(),
            NativePluginRegistry::new().with_factory(ConstructionExitFactory {
                object: object.clone(),
                stopped: stopped.clone(),
                exit: ConstructionExit::CancelAfterReturn,
            }),
        ))
        .expect_err("cancelled construction must fail startup");

    assert_eq!(error, RuntimeFailure::AdmissionClosed);
    assert_eq!(
        object
            .get()
            .expect("late returned object must remain Host-owned")
            .configuration,
        r#"{"mode":"exit"}"#
    );
    assert_eq!(*stopped.borrow(), 1);
}

#[derive(Debug)]
struct NonCooperativeStopFactory {
    object: PluginObject<NonClonePlugin>,
    stop_attempts: Rc<RefCell<usize>>,
}

impl NativePluginFactory for NonCooperativeStopFactory {
    fn package_id(&self) -> &'static str {
        "test.non-cooperative-stop"
    }

    fn runtime_profile(&self) -> &'static str {
        "lenso.native-authoring@2"
    }

    fn instantiate(
        &self,
        context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        let configuration = context.configuration().to_owned();
        let stop_attempts = self.stop_attempts.clone();
        let lifecycle = CompleteObjectLifecycle::new(
            self.object.clone(),
            context.configuration(),
            move |_context| {
                let configuration = configuration.clone();
                Box::pin(async move { Ok(Rc::new(NonClonePlugin { configuration })) })
            },
        )
        .with_stop(move |_object, _lifecycle| {
            *stop_attempts.borrow_mut() += 1;
            Box::pin(futures::future::pending())
        });
        Ok(NativePluginInstance::with_lifecycle(Vec::new(), lifecycle))
    }
}

#[test]
fn native_noncooperation_reports_timeout_without_claiming_physical_termination() {
    let object = PluginObject::empty();
    let stop_attempts = Rc::new(RefCell::new(0));
    let driver = DeterministicDriver::new();
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("native", "test.non-cooperative-stop")
                .with_authoring(2, "lenso.native-authoring@2")
                .with_configuration(r#"{"mode":"non-cooperative"}"#),
        ],
        vec![],
    );
    let app = driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new().with_factory(NonCooperativeStopFactory {
                object: object.clone(),
                stop_attempts: stop_attempts.clone(),
            }),
        ))
        .expect("the non-cooperative fixture should start");

    let timer_driver = driver.clone();
    let shutdown = app.shutdown(Duration::from_millis(1));
    let advance_timeout = async move {
        timer_driver.yield_now().await;
        timer_driver.advance(Duration::from_millis(1));
    };
    let (outcome, ()) = driver.run(futures::future::join(shutdown, advance_timeout));

    assert_eq!(outcome, ShutdownOutcome::Timeout);
    assert_eq!(*stop_attempts.borrow(), 1);
    assert!(object.get().is_ok());
}

#[derive(Debug, serde::Deserialize, lenso_native_adapter::PluginConfig)]
struct MacroConfig {
    value: String,
}

#[derive(Debug)]
struct NonDefaultResource(String);

#[lenso_native_adapter::plugin]
#[derive(Debug)]
struct MacroConstructedPlugin {
    #[config]
    config: MacroConfig,
    resource: NonDefaultResource,
}

static MACRO_STOPPED: AtomicUsize = AtomicUsize::new(0);

#[lenso_native_adapter::plugin_impl]
impl MacroConstructedPlugin {
    #[create]
    async fn create(
        config: MacroConfig,
        #[lifecycle] lifecycle: LifecycleContext,
    ) -> Result<Self, &'static str> {
        std::future::ready(()).await;
        if lifecycle.cancellation().is_cancelled() {
            return Err("construction was cancelled");
        }
        let resource = NonDefaultResource(config.value.clone());
        Ok(Self { config, resource })
    }

    #[stop]
    fn stop(&self, #[lifecycle] lifecycle: LifecycleContext) -> Result<(), &'static str> {
        let remaining_budget = lifecycle.remaining_budget();
        drop(lifecycle);
        if remaining_budget.is_none() {
            return Err("stop had no Host budget");
        }
        assert_eq!(self.config.value, self.resource.0);
        MACRO_STOPPED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Debug)]
struct MacroFactory {
    object: PluginObject<MacroConstructedPlugin>,
}

impl NativePluginFactory for MacroFactory {
    fn package_id(&self) -> &'static str {
        "test.macro-complete-object"
    }

    fn runtime_profile(&self) -> &'static str {
        "lenso.native-authoring@2"
    }

    fn instantiate(
        &self,
        context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        let lifecycle =
            CompleteObjectLifecycle::linked(self.object.clone(), context.configuration())?;
        Ok(NativePluginInstance::with_lifecycle(Vec::new(), lifecycle))
    }
}

#[test]
fn plugin_impl_constructs_a_non_default_non_clone_complete_object() {
    MACRO_STOPPED.store(0, Ordering::SeqCst);
    let object = PluginObject::empty();
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("macro", "test.macro-complete-object")
                .with_authoring(2, "lenso.native-authoring@2")
                .with_configuration(r#"{"value":"owned"}"#),
        ],
        vec![],
    );
    let driver = DeterministicDriver::new();
    let app = driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new().with_factory(MacroFactory {
                object: object.clone(),
            }),
        ))
        .expect("generated custom constructor should create the complete object");

    let plugin = object
        .get()
        .expect("generated constructor should install object");
    assert_eq!(plugin.config.value, "owned");
    assert_eq!(plugin.resource.0, "owned");
    assert_eq!(
        driver.run(app.shutdown(Duration::from_secs(1))),
        ShutdownOutcome::Clean
    );
    assert_eq!(MACRO_STOPPED.load(Ordering::SeqCst), 1);
}

mod sync_constructor {
    use super::{AtomicUsize, LifecycleContext, Ordering};

    #[derive(Debug)]
    pub struct Resource(pub &'static str);

    #[lenso_native_adapter::plugin]
    #[derive(Debug)]
    pub struct Plugin {
        pub resource: Resource,
    }

    pub static STOPPED: AtomicUsize = AtomicUsize::new(0);

    #[lenso_native_adapter::plugin_impl]
    impl Plugin {
        #[create]
        fn create() -> Self {
            Self {
                resource: Resource("sync"),
            }
        }

        #[stop]
        async fn stop(&self, #[lifecycle] lifecycle: LifecycleContext) {
            assert_eq!(self.resource.0, "sync");
            assert!(lifecycle.remaining_budget().is_some());
            std::future::ready(()).await;
            STOPPED.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Debug)]
struct SyncConstructorFactory {
    object: PluginObject<sync_constructor::Plugin>,
}

impl NativePluginFactory for SyncConstructorFactory {
    fn package_id(&self) -> &'static str {
        "test.sync-constructor"
    }

    fn runtime_profile(&self) -> &'static str {
        "lenso.native-authoring@2"
    }

    fn instantiate(
        &self,
        context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        let lifecycle =
            CompleteObjectLifecycle::linked(self.object.clone(), context.configuration())?;
        Ok(NativePluginInstance::with_lifecycle(Vec::new(), lifecycle))
    }
}

#[test]
fn plugin_impl_accepts_sync_create_and_async_stop() {
    sync_constructor::STOPPED.store(0, Ordering::SeqCst);
    let object = PluginObject::empty();
    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("sync", "test.sync-constructor")
                .with_authoring(2, "lenso.native-authoring@2")
                .with_configuration("{}"),
        ],
        vec![],
    );
    let driver = DeterministicDriver::new();
    let app = driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new().with_factory(SyncConstructorFactory {
                object: object.clone(),
            }),
        ))
        .expect("sync custom constructor should create the complete object");

    assert_eq!(
        object.get().expect("object should be installed").resource.0,
        "sync"
    );
    assert_eq!(
        driver.run(app.shutdown(Duration::from_secs(1))),
        ShutdownOutcome::Clean
    );
    assert_eq!(sync_constructor::STOPPED.load(Ordering::SeqCst), 1);
}
