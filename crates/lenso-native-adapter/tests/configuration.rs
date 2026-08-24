use std::{cell::RefCell, rc::Rc, time::Duration};

use lenso_app_plan::{ModuleInstancePlan, ResolvedAppPlan, RestartPolicy};
use lenso_kernel::{DeterministicDriver, Kernel, RuntimeDriver, RuntimeFailure};
use lenso_native_adapter::{
    NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance, NativeModuleRegistry,
};

#[derive(Debug)]
struct RecordingFactory {
    observed: Rc<RefCell<Vec<(String, String, String)>>>,
}

impl NativeModuleFactory for RecordingFactory {
    fn package_id(&self) -> &'static str {
        "test.configured"
    }
    fn package_version(&self) -> &'static str {
        "1.0.0"
    }

    fn instantiate(
        &self,
        context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        self.observed.borrow_mut().push((
            context.instance_key().to_owned(),
            context.entrypoint().to_owned(),
            context.configuration().to_owned(),
        ));
        Ok(NativeModuleInstance::default())
    }
}

#[derive(Debug)]
struct PluginFactory {
    observed: Rc<RefCell<Vec<(String, String, String)>>>,
}

impl NativeModuleFactory for PluginFactory {
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
        context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        self.observed.borrow_mut().push((
            context.instance_key().to_owned(),
            context.entrypoint().to_owned(),
            context.configuration().to_owned(),
        ));
        Ok(NativeModuleInstance::default())
    }
}

#[test]
fn native_factory_version_must_match_the_authoring_resolved_version() {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let plan = ResolvedAppPlan::new(
        vec![
            ModuleInstancePlan::new("configured", "test.configured").with_package_revision("2.0.0"),
        ],
        vec![],
    );
    let driver = DeterministicDriver::new();
    let error = driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativeModuleRegistry::new().with_factory(RecordingFactory { observed }),
        ))
        .expect_err("a differently linked Cargo package must be rejected");
    assert!(matches!(error, RuntimeFailure::MissingModuleFactory { .. }));
}

#[test]
fn native_factory_identity_matches_a_plugin_resolved_revision() {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let plan = ResolvedAppPlan::new(
        vec![
            ModuleInstancePlan::new("configured", "test.configured")
                .with_package_revision("test.configured@host-build-a"),
        ],
        vec![],
    );
    let driver = DeterministicDriver::new();

    driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativeModuleRegistry::new().with_factory(PluginFactory {
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
fn native_factory_receives_the_exact_immutable_instance_input() {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let plan = ResolvedAppPlan::new(
        vec![
            ModuleInstancePlan::new("configured", "test.configured")
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
            NativeModuleRegistry::new().with_factory(RecordingFactory {
                observed: observed.clone(),
            }),
        ))
        .expect("the configured native App should start");
    app.report_module_failure("configured")
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
