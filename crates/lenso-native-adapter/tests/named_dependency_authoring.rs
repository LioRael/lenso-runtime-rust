#![allow(dead_code)]

use lenso_native_adapter::plugin;
use lenso_plugin_authoring::{BoundCapabilityClient, CapabilityClient, CapabilityClientMany};

mod store {
    #[macro_export]
    macro_rules! __test_lenso_required_store_client {
        ($requirement_id:literal) => {
            concat!(
                "{\"requirement_id\":",
                stringify!($requirement_id),
                ",\"capability_id\":\"example.store@1\",\"descriptor_version\":\"1.0.0\",\"cardinality\":\"one\"}"
            )
        };
    }
    pub use crate::__test_lenso_required_store_client as __lenso_required_store_client;

    #[macro_export]
    macro_rules! __test_lenso_required_optional_store_client {
        ($requirement_id:literal) => {
            concat!(
                "{\"requirement_id\":",
                stringify!($requirement_id),
                ",\"capability_id\":\"example.store@1\",\"descriptor_version\":\"1.0.0\",\"cardinality\":\"optional\"}"
            )
        };
    }
    pub use crate::__test_lenso_required_optional_store_client as __lenso_required_optional_store_client;

    #[macro_export]
    macro_rules! __test_lenso_required_many_store_client {
        ($requirement_id:literal) => {
            concat!(
                "{\"requirement_id\":",
                stringify!($requirement_id),
                ",\"capability_id\":\"example.store@1\",\"descriptor_version\":\"1.0.0\",\"cardinality\":\"many\"}"
            )
        };
    }
    pub use crate::__test_lenso_required_many_store_client as __lenso_required_many_store_client;

    #[derive(Debug)]
    pub struct StoreClient;

    impl super::CapabilityClient for StoreClient {
        type Dependencies = lenso_kernel::PluginDependencies;
        type Error = lenso_kernel::RuntimeFailure;

        const CAPABILITY_ID: &'static str = "example.store@1";
        const DESCRIPTOR_VERSION: &'static str = "1.0.0";

        fn from_dependencies(_dependencies: &Self::Dependencies) -> Result<Self, Self::Error> {
            Ok(Self)
        }

        fn already_connected() -> Self::Error {
            lenso_kernel::RuntimeFailure::PluginFailure {
                detail: "Store dependency was connected more than once".to_owned(),
            }
        }
    }

    impl super::CapabilityClientMany for StoreClient {
        fn many_from_dependencies(
            _dependencies: &Self::Dependencies,
        ) -> Result<Vec<super::BoundCapabilityClient<Self>>, Self::Error> {
            Ok(Vec::new())
        }
    }
}

#[plugin(consumer)]
#[derive(Debug)]
struct NamedDependencies {
    #[dependency(id = "source")]
    source: store::StoreClient,
    #[dependency(id = "cache")]
    cache: Option<store::StoreClient>,
    #[dependency(id = "replicas")]
    replicas: Vec<BoundCapabilityClient<store::StoreClient>>,
}

#[test]
fn same_client_type_can_fill_three_named_dependency_roles() {
    let descriptor: serde_json::Value =
        serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).expect("descriptor should be valid JSON");

    assert_eq!(descriptor["authoring_version"], 2);
    assert_eq!(descriptor["runtime_profile"], "lenso.native-authoring@2");
    assert_eq!(
        descriptor["required_capabilities"],
        serde_json::json!([
            {
                "requirement_id": "source",
                "capability_id": "example.store@1",
                "descriptor_version": "1.0.0",
                "cardinality": "one"
            },
            {
                "requirement_id": "cache",
                "capability_id": "example.store@1",
                "descriptor_version": "1.0.0",
                "cardinality": "optional"
            },
            {
                "requirement_id": "replicas",
                "capability_id": "example.store@1",
                "descriptor_version": "1.0.0",
                "cardinality": "many"
            }
        ])
    );
}
