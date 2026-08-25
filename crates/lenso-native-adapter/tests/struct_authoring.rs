#![allow(dead_code)]

use lenso_module_authoring::{
    BoundCapabilityClient, CapabilityClient, CapabilityClientMany, ManyPort,
};
use lenso_native_adapter::{Lifecycle, NativeModuleRegistry, module, provides};

mod echo {
    #[macro_export]
    macro_rules! __test_lenso_provided_echo {
        () => {
            r#"{"capability_id":"example.echo@1","descriptor_version":"1.0.0","operations":["echo"],"operation_kinds":{},"default_admission":{"queue_capacity":0,"max_concurrency":1},"operation_admissions":{},"event_admission":null,"cross_lane_transfer":false}"#
        };
    }
    pub use crate::__test_lenso_provided_echo as __lenso_provided_echo;

    #[macro_export]
    macro_rules! __test_lenso_required_many_echo_client {
        () => {
            r#"{"capability_id":"example.echo@1","descriptor_version":"1.0.0","cardinality":"many"}"#
        };
    }
    pub use crate::__test_lenso_required_many_echo_client as __lenso_required_many_echo_client;

    #[macro_export]
    macro_rules! __test_lenso_native_endpoints_echo {
        ($provider:expr, $support:path) => {{
            use $support as __LensoNativeSupport;
            let _ = $provider;
            (
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeRequestEndpoint>>::new(),
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeStreamEndpoint>>::new(),
                Vec::<std::rc::Rc<dyn __LensoNativeSupport::NativeEventEndpoint>>::new(),
            )
        }};
    }
    pub use crate::__test_lenso_native_endpoints_echo as __lenso_native_endpoints_echo;

    pub struct Echo;

    pub trait EchoProvider {}

    #[derive(Debug)]
    pub struct EchoClient;

    impl super::CapabilityClient for EchoClient {
        type Dependencies = lenso_kernel::ModuleDependencies;
        type Error = lenso_kernel::RuntimeFailure;

        const CAPABILITY_ID: &'static str = "example.echo@1";
        const DESCRIPTOR_VERSION: &'static str = "1.0.0";

        fn from_dependencies(_dependencies: &Self::Dependencies) -> Result<Self, Self::Error> {
            Ok(Self)
        }

        fn already_connected() -> Self::Error {
            lenso_kernel::RuntimeFailure::ModuleFailure {
                detail: "Echo Port was connected more than once".to_owned(),
            }
        }
    }

    impl super::CapabilityClientMany for EchoClient {
        fn many_from_dependencies(
            _dependencies: &Self::Dependencies,
        ) -> Result<Vec<super::BoundCapabilityClient<Self>>, Self::Error> {
            Ok(Vec::new())
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize, lenso_native_adapter::ModuleConfig)]
struct ExampleConfig {
    name: String,
    retries: u32,
    tags: Option<Vec<String>>,
}

fn validate(configuration: &ExampleConfig) -> Result<(), lenso_native_adapter::RuntimeFailure> {
    if configuration.name.is_empty() {
        return Err(lenso_native_adapter::RuntimeFailure::InvalidResolvedPlan {
            detail: "name must not be empty".to_owned(),
        });
    }
    Ok(())
}

#[module(validate = validate, lifecycle)]
#[derive(Clone, Debug)]
struct ExampleModule {
    #[config]
    config: ExampleConfig,
    echoes: ManyPort<echo::EchoClient>,
}

impl Lifecycle for ExampleModule {
    async fn activate(
        &self,
        _context: lenso_kernel::ActivateContext,
    ) -> Result<(), lenso_native_adapter::RuntimeFailure> {
        std::future::ready(()).await;
        Ok(())
    }
}

#[provides(echo::Echo)]
impl echo::EchoProvider for ExampleModule {}

#[test]
fn struct_module_derives_descriptor_factory_and_configuration() {
    let descriptor: serde_json::Value =
        serde_json::from_str(MODULE_DESCRIPTOR_JSON).expect("descriptor should be valid JSON");
    assert_eq!(descriptor["package_id"], "lenso.native-adapter");
    assert_eq!(
        descriptor["provided_capabilities"][0]["capability_id"],
        "example.echo@1"
    );
    assert_eq!(
        descriptor["required_capabilities"],
        serde_json::json!([{
            "capability_id": "example.echo@1",
            "descriptor_version": "1.0.0",
            "cardinality": "many"
        }])
    );
    assert_eq!(descriptor["configuration_schema"]["type"], "object");
    assert_eq!(
        descriptor["configuration_schema"]["required"],
        serde_json::json!(["name", "retries"])
    );
    assert_eq!(
        descriptor["configuration_schema"]["properties"]["tags"]["items"]["type"],
        "string"
    );

    let registry = NativeModuleRegistry::new().with_linked_factories();
    assert!(registry.factories().any(|factory| {
        factory.package_id() == "lenso.native-adapter"
            && factory.package_version() == env!("CARGO_PKG_VERSION")
    }));
}
