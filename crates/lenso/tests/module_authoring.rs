#![allow(dead_code)]

use lenso::prelude::*;

#[doc(hidden)]
pub mod __lenso_native_support {
    pub use lenso::__private::{
        NativeEventEndpoint, NativeModuleInstance, NativeRequestEndpoint, NativeStreamEndpoint,
    };
}

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
        ($provider:expr, $lifecycle:expr, $support:path) => {{
            use $support as __LensoNativeSupport;
            let _ = $provider;
            __LensoNativeSupport::NativeModuleInstance::with_all_endpoints(
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

#[derive(Clone, Debug, serde::Deserialize, ModuleConfig)]
struct ExampleConfig {
    message: String,
}

#[module]
#[derive(Clone, Debug)]
struct ExampleModule {
    #[config]
    config: ExampleConfig,
}

#[provides(echo::Echo)]
impl echo::EchoProvider for ExampleModule {}

#[test]
fn facade_owns_the_module_authoring_surface() {
    let descriptor: lenso::__private::serde_json::Value =
        lenso::__private::serde_json::from_str(MODULE_DESCRIPTOR_JSON)
            .expect("generated Descriptor should be valid JSON");

    assert_eq!(descriptor["package_id"], "lenso");
    assert_eq!(
        descriptor["provided_capabilities"][0]["capability_id"],
        "example.echo@1"
    );
}
