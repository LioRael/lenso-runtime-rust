#![allow(dead_code)]

use std::cell::Cell;

use lenso::prelude::*;
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    PluginInstancePlan,
};
use lenso_kernel::{DeterministicDriver, Kernel, NativeExecutionAdapter};
use lenso_native_adapter::{
    NativePluginFactory, NativePluginFactoryContext, NativePluginInstance, NativePluginRegistry,
};

#[derive(Debug)]
struct ConsumerFactory;

impl NativePluginFactory for ConsumerFactory {
    fn package_id(&self) -> &'static str {
        "fixture.consumer"
    }

    fn runtime_profile(&self) -> &'static str {
        "lenso.native-authoring@2"
    }

    fn instantiate(
        &self,
        _context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, lenso_kernel::RuntimeFailure> {
        Ok(NativePluginInstance::default())
    }
}

#[derive(Clone, Debug, PluginConfig)]
struct PluginSettings {
    enabled: bool,
}

#[doc(hidden)]
pub mod __lenso_native_support {
    pub use lenso::__private::{
        NativeEventEndpoint, NativePluginInstance, NativeRequestEndpoint, NativeStreamEndpoint,
    };
}

mod health {
    use std::any::Any;

    use lenso::__private::{
        InvocationContext, LocalBoxFuture, NativeRequestEndpoint, NativeRequestFuture,
        RuntimeFailure,
    };
    use lenso_kernel::RequestCapability;

    #[derive(Debug)]
    pub struct HealthEndpoint {
        provider: std::rc::Rc<dyn HealthProvider>,
    }

    impl HealthEndpoint {
        pub(crate) fn new(provider: impl HealthProvider) -> Self {
            Self {
                provider: std::rc::Rc::new(provider),
            }
        }
    }

    impl NativeRequestEndpoint for HealthEndpoint {
        fn capability_id(&self) -> &'static str {
            "example.health@1"
        }

        fn descriptor_version(&self) -> &'static str {
            "1.0.0"
        }

        fn operations(&self) -> &'static [&'static str] {
            &["check"]
        }

        fn invoke(
            &self,
            _operation: &str,
            request: Box<dyn Any>,
            context: InvocationContext,
        ) -> LocalBoxFuture<'static, Result<Result<Box<dyn Any>, Box<dyn Any>>, RuntimeFailure>>
        {
            let Ok(request) = request.downcast::<()>() else {
                return Box::pin(async {
                    Err(RuntimeFailure::ProtocolViolation {
                        capability: "example.health@1",
                    })
                });
            };
            let _: () = *request;
            let invocation = self.provider.check(context, ());
            Box::pin(async move {
                invocation.await.map(|result| {
                    result
                        .map(|value| Box::new(value) as Box<dyn Any>)
                        .map_err(|error| Box::new(error) as Box<dyn Any>)
                })
            })
        }
    }

    #[macro_export]
    macro_rules! __test_lenso_provided_health {
        () => {
            r#"{"capability_id":"example.health@1","descriptor_version":"1.0.0","operations":["check"],"operation_kinds":{},"default_admission":{"queue_capacity":0,"max_concurrency":1},"operation_admissions":{},"event_admission":null,"cross_lane_transfer":false}"#
        };
    }
    pub use crate::__test_lenso_provided_health as __lenso_provided_health;

    #[macro_export]
    macro_rules! __test_lenso_native_endpoints_health {
        ($provider:expr, $support:path) => {{
            use $support as __LensoNativeSupport;
            (
                vec![
                    std::rc::Rc::new($crate::health::HealthEndpoint::new($provider))
                        as std::rc::Rc<dyn __LensoNativeSupport::NativeRequestEndpoint>,
                ],
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeStreamEndpoint>>::new(),
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeEventEndpoint>>::new(),
            )
        }};
    }
    pub use crate::__test_lenso_native_endpoints_health as __lenso_native_endpoints_health;

    #[macro_export]
    macro_rules! __test_lenso_native_lower_health {
        ($plugin:ty, $support:path) => {
            use $support as __LensoNativeSupportHealth;
            impl $crate::health::HealthProvider for $plugin {
                fn check(
                    &self,
                    context: __LensoNativeSupportHealth::InvocationContext,
                    request: (),
                ) -> __LensoNativeSupportHealth::NativeRequestFuture<$crate::health::Health> {
                    let plugin = self.clone();
                    Box::pin(async move { Ok(<$plugin>::check(&plugin, context, request).await) })
                }
            }
        };
    }
    pub use crate::__test_lenso_native_lower_health as __lenso_native_lower_health;

    #[macro_export]
    macro_rules! __test_lenso_native_lower_object_health {
        ($object:ty, $plugin:ty, $support:path) => {
            use $support as __LensoNativeObjectSupportHealth;
            impl $crate::health::HealthProvider for $object {
                fn check(
                    &self,
                    context: __LensoNativeObjectSupportHealth::InvocationContext,
                    request: (),
                ) -> __LensoNativeObjectSupportHealth::NativeRequestFuture<$crate::health::Health>
                {
                    let object = self.clone();
                    Box::pin(async move {
                        let plugin = object.get()?;
                        Ok(<$plugin>::check(plugin.as_ref(), context, request).await)
                    })
                }
            }
        };
    }
    pub use crate::__test_lenso_native_lower_object_health as __lenso_native_lower_object_health;

    #[derive(Debug)]
    pub struct Health;

    impl RequestCapability for Health {
        type Request = ();
        type Response = usize;
        type DomainError = ();

        const ID: &'static str = "example.health@1";
        const DESCRIPTOR_VERSION: &'static str = "1.0.0";
    }

    pub trait HealthProvider: std::fmt::Debug + 'static {
        fn check(&self, context: InvocationContext, request: ()) -> NativeRequestFuture<Health>;
    }
}

#[plugin]
#[derive(Clone, Debug, Default)]
struct StatelessHealthPlugin {
    calls: Cell<usize>,
}

#[provides(health::Health)]
impl StatelessHealthPlugin {
    async fn check(
        &self,
        _context: lenso::__private::InvocationContext,
        (): (),
    ) -> Result<usize, ()> {
        let calls = self.calls.get() + 1;
        self.calls.set(calls);
        Ok(calls)
    }
}

#[test]
fn stateless_plugin_derives_empty_configuration() {
    let descriptor: lenso::__private::serde_json::Value =
        lenso::__private::serde_json::from_str(PLUGIN_DESCRIPTOR_JSON)
            .expect("generated Descriptor should be valid JSON");
    assert_eq!(
        descriptor["configuration_schema"],
        lenso::__private::serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "required": [],
            "properties": {},
        })
    );
    assert_eq!(descriptor["authoring_version"], 2);
    assert_eq!(descriptor["runtime_profile"], "lenso.native-authoring@2");

    let provider = PluginInstancePlan::new("health", "lenso")
        .with_authoring(2, "lenso.native-authoring@2")
        .with_configuration("{}")
        .with_capability(CapabilityEndpointPlan::new(
            "example.health@1",
            "1.0.0",
            ["check"],
        ));
    let plan = AppComposition::new(vec![provider], vec![])
        .resolve()
        .expect("stateless plan should resolve");
    NativeExecutionAdapter::prepare(&NativePluginRegistry::new().with_linked_factories(), &plan)
        .expect("stateless Plugin should accept empty configuration");
}

#[test]
fn authoring_v2_provider_invokes_the_one_constructed_object() {
    let provider = PluginInstancePlan::new("health", "lenso")
        .with_authoring(2, "lenso.native-authoring@2")
        .with_configuration("{}")
        .with_capability(CapabilityEndpointPlan::new(
            "example.health@1",
            "1.0.0",
            ["check"],
        ));
    let consumer = PluginInstancePlan::new("consumer", "fixture.consumer")
        .with_authoring(2, "lenso.native-authoring@2")
        .with_requirement(
            CapabilityRequirementPlan::one("example.health@1", "1.0.0")
                .with_requirement_id("health"),
        );
    let plan = AppComposition::new(
        vec![consumer, provider],
        vec![
            CapabilityBinding::new("consumer", "example.health@1", "1.0.0", "health")
                .with_requirement_id("health"),
        ],
    )
    .resolve()
    .expect("complete-object provider plan should resolve");
    let driver = DeterministicDriver::new();
    let app = driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new()
                .with_linked_factories()
                .with_factory(ConsumerFactory),
        ))
        .expect("authoring v2 provider should construct");

    assert_eq!(
        driver
            .run(app.invoke::<health::Health>("consumer", "check", ()))
            .expect("first request should reach the provider"),
        Ok(1)
    );
    assert_eq!(
        driver
            .run(app.invoke::<health::Health>("consumer", "check", ()))
            .expect("second request should reach the provider"),
        Ok(2)
    );
}

#[test]
fn stateless_plugin_rejects_non_empty_configuration() {
    let provider = PluginInstancePlan::new("health", "lenso")
        .with_authoring(2, "lenso.native-authoring@2")
        .with_configuration(r#"{"unexpected":true}"#)
        .with_capability(CapabilityEndpointPlan::new(
            "example.health@1",
            "1.0.0",
            ["check"],
        ));
    let plan = AppComposition::new(vec![provider], vec![])
        .resolve()
        .expect("composition resolution defers Plugin-owned configuration validation");
    let driver = DeterministicDriver::new();
    let error = driver
        .run(Kernel::start_native(
            plan,
            driver.clone(),
            NativePluginRegistry::new().with_linked_factories(),
        ))
        .expect_err("stateless Plugin must reject non-empty configuration");
    assert!(format!("{error:?}").contains("does not accept configuration"));
}
