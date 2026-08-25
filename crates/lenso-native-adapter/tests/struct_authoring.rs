#![allow(dead_code)]

use lenso_native_adapter::{NativeModuleRegistry, module, provides};

mod echo {
    #[macro_export]
    macro_rules! __test_lenso_provided_echo {
        () => {
            r#"{"capability_id":"example.echo@1","descriptor_version":"1.0.0","operations":["echo"],"operation_kinds":{},"default_admission":{"queue_capacity":0,"max_concurrency":1},"operation_admissions":{},"event_admission":null,"cross_lane_transfer":false}"#
        };
    }
    pub use crate::__test_lenso_provided_echo as __lenso_provided_echo;

    #[macro_export]
    macro_rules! __test_lenso_native_provide_echo {
        ($provider:expr, $lifecycle:expr) => {{
            let _ = $provider;
            ::lenso_native_adapter::NativeModuleInstance::with_all_endpoints(
                Vec::new(),
                Vec::new(),
                Vec::new(),
                $lifecycle,
            )
        }};
    }
    pub use crate::__test_lenso_native_provide_echo as __lenso_native_provide_echo;

    pub struct Echo;

    pub trait EchoProvider {}
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

#[module(validate = validate)]
#[derive(Clone, Debug)]
struct ExampleModule {
    #[config]
    config: ExampleConfig,
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
    assert_eq!(descriptor["required_capabilities"], serde_json::json!([]));
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
