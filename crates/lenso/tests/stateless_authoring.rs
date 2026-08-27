#![allow(dead_code)]

use lenso::prelude::*;
use lenso_app_plan::{AppComposition, CapabilityEndpointPlan, PluginInstancePlan};
use lenso_kernel::NativeExecutionAdapter;
use lenso_native_adapter::NativePluginRegistry;

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
        InvocationContext, LocalBoxFuture, NativeRequestEndpoint, RuntimeFailure,
    };

    #[derive(Debug)]
    pub struct HealthEndpoint;

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
            _request: Box<dyn Any>,
            _context: InvocationContext,
        ) -> LocalBoxFuture<'static, Result<Result<Box<dyn Any>, Box<dyn Any>>, RuntimeFailure>>
        {
            Box::pin(async { Ok(Ok(Box::new(()) as Box<dyn Any>)) })
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
            let _ = $provider;
            (
                vec![std::rc::Rc::new($crate::health::HealthEndpoint)
                    as std::rc::Rc<
                        dyn __LensoNativeSupport::NativeRequestEndpoint,
                    >],
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeStreamEndpoint>>::new(),
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeEventEndpoint>>::new(),
            )
        }};
    }
    pub use crate::__test_lenso_native_endpoints_health as __lenso_native_endpoints_health;

    #[macro_export]
    macro_rules! __test_lenso_native_lower_health {
        ($module:ty, $support:path) => {
            impl $crate::health::HealthProvider for $module {}
        };
    }
    pub use crate::__test_lenso_native_lower_health as __lenso_native_lower_health;

    pub struct Health;
    pub trait HealthProvider {}
}

#[plugin]
#[derive(Clone, Debug, Default)]
struct StatelessHealthPlugin {}

#[provides(health::Health)]
impl StatelessHealthPlugin {}

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

    let provider = PluginInstancePlan::new("health", "lenso")
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
fn stateless_plugin_rejects_non_empty_configuration() {
    let provider = PluginInstancePlan::new("health", "lenso")
        .with_configuration(r#"{"unexpected":true}"#)
        .with_capability(CapabilityEndpointPlan::new(
            "example.health@1",
            "1.0.0",
            ["check"],
        ));
    let plan = AppComposition::new(vec![provider], vec![])
        .resolve()
        .expect("composition resolution defers Plugin-owned configuration validation");
    let error = NativeExecutionAdapter::prepare(
        &NativePluginRegistry::new().with_linked_factories(),
        &plan,
    )
    .expect_err("stateless Plugin must reject non-empty configuration");
    assert!(format!("{error:?}").contains("does not accept configuration"));
}
