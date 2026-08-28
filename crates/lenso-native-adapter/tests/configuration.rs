use std::{cell::RefCell, rc::Rc, time::Duration};

use lenso_app_plan::{PluginInstancePlan, ResolvedAppPlan, RestartPolicy};
use lenso_kernel::{DeterministicDriver, Kernel, RuntimeDriver, RuntimeFailure};
use lenso_native_adapter::{
    InstanceResources, NativePluginFactory, NativePluginFactoryContext, NativePluginInstance,
    NativePluginRegistry,
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
