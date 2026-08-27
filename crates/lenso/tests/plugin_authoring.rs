#![allow(dead_code)]

use lenso::prelude::*;
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    ExecutionClassId, PluginInstancePlan,
};
use lenso_kernel::NativeExecutionAdapter;
use lenso_native_adapter::NativePluginRegistry;

#[doc(hidden)]
pub mod __lenso_native_support {
    pub use lenso::__private::{
        NativeEventEndpoint, NativePluginInstance, NativeRequestEndpoint, NativeStreamEndpoint,
    };
}

mod echo {
    use std::any::Any;

    use lenso::__private::{
        InvocationContext, LocalBoxFuture, NativeRequestEndpoint, RuntimeFailure,
    };

    #[derive(Debug)]
    pub struct EchoEndpoint;

    impl NativeRequestEndpoint for EchoEndpoint {
        fn capability_id(&self) -> &'static str {
            "example.echo@1"
        }

        fn descriptor_version(&self) -> &'static str {
            "1.0.0"
        }

        fn operations(&self) -> &'static [&'static str] {
            &["echo"]
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
    macro_rules! __test_lenso_provided_echo {
        () => {
            r#"{"capability_id":"example.echo@1","descriptor_version":"1.0.0","operations":["echo"],"operation_kinds":{},"default_admission":{"queue_capacity":0,"max_concurrency":1},"operation_admissions":{},"event_admission":null,"cross_lane_transfer":false}"#
        };
    }
    pub use crate::__test_lenso_provided_echo as __lenso_provided_echo;

    #[macro_export]
    macro_rules! __test_lenso_native_endpoints_echo {
        ($provider:expr, $support:path) => {{
            use $support as __LensoNativeSupport;
            let _ = $provider;
            (
                vec![std::rc::Rc::new($crate::echo::EchoEndpoint)
                    as std::rc::Rc<
                        dyn __LensoNativeSupport::NativeRequestEndpoint,
                    >],
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeStreamEndpoint>>::new(),
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeEventEndpoint>>::new(),
            )
        }};
    }
    pub use crate::__test_lenso_native_endpoints_echo as __lenso_native_endpoints_echo;

    #[macro_export]
    macro_rules! __test_lenso_native_lower_echo {
        ($module:ty, $support:path) => {
            impl $crate::echo::EchoProvider for $module {}
        };
    }
    pub use crate::__test_lenso_native_lower_echo as __lenso_native_lower_echo;

    pub struct Echo;
    pub trait EchoProvider {}
}

mod conversation {
    use std::any::Any;

    use lenso::__private::{
        InvocationContext, LocalBoxFuture, NativeStreamEndpoint, NativeStreamSession,
        RuntimeFailure,
    };

    #[derive(Debug)]
    pub struct ConversationEndpoint;

    impl NativeStreamEndpoint for ConversationEndpoint {
        fn capability_id(&self) -> &'static str {
            "example.conversation@1"
        }

        fn descriptor_version(&self) -> &'static str {
            "1.0.0"
        }

        fn operations(&self) -> &'static [&'static str] {
            &["open"]
        }

        fn open(
            &self,
            _operation: &str,
            _request: Box<dyn Any>,
            _context: InvocationContext,
        ) -> LocalBoxFuture<
            'static,
            Result<Result<Box<dyn NativeStreamSession>, Box<dyn Any>>, RuntimeFailure>,
        > {
            Box::pin(async {
                Err(RuntimeFailure::PluginFailure {
                    detail: "fixture stream is never opened".to_owned(),
                })
            })
        }
    }

    #[macro_export]
    macro_rules! __test_lenso_provided_conversation {
        () => {
            r#"{"capability_id":"example.conversation@1","descriptor_version":"1.0.0","operations":["open"],"operation_kinds":{"open":"stream"},"default_admission":{"queue_capacity":0,"max_concurrency":1},"operation_admissions":{},"event_admission":null,"cross_lane_transfer":false}"#
        };
    }
    pub use crate::__test_lenso_provided_conversation as __lenso_provided_conversation;

    #[macro_export]
    macro_rules! __test_lenso_native_endpoints_conversation {
        ($provider:expr, $support:path) => {{
            use $support as __LensoNativeSupport;
            let _ = $provider;
            (
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeRequestEndpoint>>::new(),
                vec![std::rc::Rc::new($crate::conversation::ConversationEndpoint)
                    as std::rc::Rc<
                        dyn __LensoNativeSupport::NativeStreamEndpoint,
                    >],
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeEventEndpoint>>::new(),
            )
        }};
    }
    pub use crate::__test_lenso_native_endpoints_conversation as __lenso_native_endpoints_conversation;

    #[macro_export]
    macro_rules! __test_lenso_native_lower_conversation {
        ($module:ty, $support:path) => {
            impl $crate::conversation::ConversationProvider for $module {}
        };
    }
    pub use crate::__test_lenso_native_lower_conversation as __lenso_native_lower_conversation;

    pub struct Conversation;
    pub trait ConversationProvider {}
}

mod audit {
    use std::any::Any;

    use lenso::__private::{
        InvocationContext, LocalBoxFuture, NativeEventEndpoint, RuntimeFailure,
    };

    #[derive(Debug)]
    pub struct AuditEndpoint;

    impl NativeEventEndpoint for AuditEndpoint {
        fn capability_id(&self) -> &'static str {
            "example.audit@1"
        }

        fn descriptor_version(&self) -> &'static str {
            "1.0.0"
        }

        fn operations(&self) -> &'static [&'static str] {
            &["record"]
        }

        fn publish(
            &self,
            _operation: &str,
            _event: Box<dyn Any>,
            _context: InvocationContext,
        ) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[macro_export]
    macro_rules! __test_lenso_provided_audit {
        () => {
            r#"{"capability_id":"example.audit@1","descriptor_version":"1.0.0","operations":["record"],"operation_kinds":{"record":"event"},"default_admission":{"queue_capacity":0,"max_concurrency":1},"operation_admissions":{},"event_admission":{"capacity":8},"cross_lane_transfer":false}"#
        };
    }
    pub use crate::__test_lenso_provided_audit as __lenso_provided_audit;

    #[macro_export]
    macro_rules! __test_lenso_native_endpoints_audit {
        ($provider:expr, $support:path) => {{
            use $support as __LensoNativeSupport;
            let _ = $provider;
            (
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeRequestEndpoint>>::new(),
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeStreamEndpoint>>::new(),
                vec![std::rc::Rc::new($crate::audit::AuditEndpoint)
                    as std::rc::Rc<dyn __LensoNativeSupport::NativeEventEndpoint>],
            )
        }};
    }
    pub use crate::__test_lenso_native_endpoints_audit as __lenso_native_endpoints_audit;

    #[macro_export]
    macro_rules! __test_lenso_native_lower_audit {
        ($module:ty, $support:path) => {
            impl $crate::audit::AuditProvider for $module {}
        };
    }
    pub use crate::__test_lenso_native_lower_audit as __lenso_native_lower_audit;

    pub struct Audit;
    pub trait AuditProvider {}
}

#[derive(Clone, Debug, serde::Deserialize, PluginConfig)]
struct ExampleConfig {
    message: String,
}

#[plugin]
#[derive(Clone, Debug)]
struct ExamplePlugin {
    #[config]
    config: ExampleConfig,
    #[tasks]
    tasks: ManagedTasks,
}

#[provides(echo::Echo, conversation::Conversation, audit::Audit)]
impl ExamplePlugin {}

#[test]
fn facade_owns_the_plugin_authoring_surface() {
    assert!(!ManagedTasks::default().is_active());

    let descriptor: lenso::__private::serde_json::Value =
        lenso::__private::serde_json::from_str(PLUGIN_DESCRIPTOR_JSON)
            .expect("generated Descriptor should be valid JSON");

    assert_eq!(descriptor["plugin_id"], "lenso");
    assert_eq!(descriptor["root_slot"], "test");
    assert_eq!(
        descriptor["provided_capabilities"][0]["capability_id"],
        "example.echo@1"
    );
    assert_eq!(
        descriptor["provided_capabilities"][1]["capability_id"],
        "example.conversation@1"
    );
    assert_eq!(
        descriptor["provided_capabilities"][2]["capability_id"],
        "example.audit@1"
    );
    assert_eq!(
        descriptor["provided_capabilities"]
            .as_array()
            .expect("provided Capabilities should be an array")
            .len(),
        3
    );

    let linked_factories = NativePluginRegistry::new()
        .with_linked_factories()
        .factories()
        .filter(|factory| factory.package_id() == "lenso")
        .count();
    assert_eq!(linked_factories, 1);
}

#[test]
fn one_factory_aggregates_request_stream_and_event_endpoints() {
    let provider = PluginInstancePlan::new("example", "lenso")
        .with_configuration(r#"{"message":"hello"}"#)
        .with_capability(CapabilityEndpointPlan::new(
            "example.echo@1",
            "1.0.0",
            ["echo"],
        ))
        .with_capability(
            CapabilityEndpointPlan::new("example.conversation@1", "1.0.0", ["open"])
                .with_stream_operation("open"),
        )
        .with_capability(
            CapabilityEndpointPlan::new("example.audit@1", "1.0.0", ["record"])
                .with_event_operation("record")
                .with_event_capacity(8),
        );
    let consumer = PluginInstancePlan::new("consumer", "fixture.consumer")
        .with_execution_class(ExecutionClassId::new("fixture.external@1"))
        .with_requirement(CapabilityRequirementPlan::one("example.echo@1", "1.0.0"))
        .with_requirement(CapabilityRequirementPlan::one(
            "example.conversation@1",
            "1.0.0",
        ))
        .with_requirement(CapabilityRequirementPlan::one("example.audit@1", "1.0.0"));
    let plan = AppComposition::new(
        vec![consumer, provider],
        vec![
            CapabilityBinding::new("consumer", "example.echo@1", "1.0.0", "example"),
            CapabilityBinding::new("consumer", "example.conversation@1", "1.0.0", "example"),
            CapabilityBinding::new("consumer", "example.audit@1", "1.0.0", "example"),
        ],
    )
    .resolve()
    .expect("mixed Capability plan should resolve");

    NativeExecutionAdapter::prepare(&NativePluginRegistry::new().with_linked_factories(), &plan)
        .expect("the one generated factory should expose all three endpoint kinds");
}
