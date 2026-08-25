#![allow(dead_code)]

use lenso::module;
use lenso_app_plan::{AppComposition, ModuleInstancePlan};
use lenso_kernel::NativeExecutionAdapter;
use lenso_native_adapter::NativeModuleRegistry;

#[module(consumer)]
#[derive(Clone, Debug, Default)]
struct ConsumerAnchor {}

#[test]
fn consumer_only_module_derives_an_empty_provider_set_and_linked_factory() {
    let descriptor: lenso::__private::serde_json::Value =
        lenso::__private::serde_json::from_str(MODULE_DESCRIPTOR_JSON)
            .expect("generated Descriptor should be valid JSON");
    assert_eq!(
        descriptor["provided_capabilities"],
        lenso::__private::serde_json::json!([])
    );
    assert_eq!(
        descriptor["required_capabilities"],
        lenso::__private::serde_json::json!([])
    );

    let consumer = ModuleInstancePlan::new("consumer", "lenso").with_configuration("{}");
    let plan = AppComposition::new(vec![consumer], vec![])
        .resolve()
        .expect("consumer-only plan should resolve");
    NativeExecutionAdapter::prepare(&NativeModuleRegistry::new().with_linked_factories(), &plan)
        .expect("consumer-only Module should have a linked factory");
}
