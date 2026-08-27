#![allow(dead_code)]

use lenso_native_adapter::module;

#[derive(Clone, Debug, serde::Deserialize)]
struct ManualConfig {
    name: String,
    retries: u32,
}

#[module(
    consumer,
    configuration_schema = "tests/fixtures/manual-config.schema.json",
    configuration_defaults = "tests/fixtures/manual-config.defaults.json"
)]
#[derive(Clone, Debug)]
struct ManualDefaultsModule {
    #[config]
    config: ManualConfig,
}

#[test]
fn explicit_package_schema_embeds_package_local_defaults() {
    let descriptor: serde_json::Value =
        serde_json::from_str(MODULE_DESCRIPTOR_JSON).expect("descriptor should be valid JSON");

    assert_eq!(
        descriptor["configuration_defaults"],
        serde_json::json!({"retries": 3})
    );
    assert_eq!(descriptor["configuration_schema"]["type"], "object");
}
