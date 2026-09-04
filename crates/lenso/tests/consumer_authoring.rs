#![allow(dead_code)]

use lenso::plugin;
use lenso_app_plan::{AppComposition, PluginInstancePlan};
use lenso_kernel::NativeExecutionAdapter;
use lenso_native_adapter::NativePluginRegistry;

#[plugin(consumer)]
#[derive(Clone, Debug, Default)]
struct ConsumerAnchor {}

#[test]
fn consumer_only_plugin_derives_an_empty_provider_set_and_linked_factory() {
    let descriptor: lenso::__private::serde_json::Value =
        lenso::__private::serde_json::from_str(PLUGIN_DESCRIPTOR_JSON)
            .expect("generated Descriptor should be valid JSON");
    assert_eq!(
        descriptor["provided_capabilities"],
        lenso::__private::serde_json::json!([])
    );
    assert_eq!(
        descriptor["required_capabilities"],
        lenso::__private::serde_json::json!([])
    );
    assert_eq!(descriptor["authoring_version"], 2);
    assert_eq!(descriptor["runtime_profile"], "lenso.native-authoring@2");

    let consumer = PluginInstancePlan::new("consumer", "lenso")
        .with_authoring(2, "lenso.native-authoring@2")
        .with_configuration("{}");
    let plan = AppComposition::new(vec![consumer], vec![])
        .resolve()
        .expect("consumer-only plan should resolve");
    NativeExecutionAdapter::prepare(&NativePluginRegistry::new().with_linked_factories(), &plan)
        .expect("consumer-only Plugin should have a linked factory");
}
