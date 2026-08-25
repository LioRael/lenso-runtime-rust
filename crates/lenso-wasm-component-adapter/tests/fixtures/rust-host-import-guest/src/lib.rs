wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

struct GuestComponent;

impl Guest for GuestComponent {
    fn describe() -> String {
        r#"{"abi":"lenso.json-host-imports@1","capabilities":[{"capability_id":"test.echo@1","descriptor_version":"1.0.0","request_operations":["echo"]}],"required_capabilities":[{"capability_id":"lenso.runtime.conformance.probe@1","descriptor_version":"1.0.0","cardinality":"one"}]}"#.to_owned()
    }

    fn invoke(
        capability: String,
        operation: String,
        request_json: String,
    ) -> Result<String, String> {
        assert_eq!(capability, "test.echo@1");
        assert_eq!(operation, "echo");
        let bindings: serde_json::Value = serde_json::from_str(&host_bindings()).unwrap();
        let binding = &bindings["ok"][0];
        assert_eq!(binding["provider_instance"], "provider");
        assert_eq!(
            binding["capability_id"],
            "lenso.runtime.conformance.probe@1"
        );
        let binding_id = binding["binding_id"].as_u64().unwrap() as u32;
        let request: serde_json::Value = serde_json::from_str(&request_json).unwrap();
        let imported: serde_json::Value = serde_json::from_str(&host_invoke(
            binding_id,
            "probe",
            &serde_json::json!({ "value": format!("wasm-{request}") }).to_string(),
        ))
        .unwrap();
        assert_eq!(imported["ok"]["value"], format!("Echo: wasm-{request}"));
        Ok(request_json)
    }

    fn stream_open(_: String, _: String, _: String) -> Result<u64, String> {
        Err("not supported".to_owned())
    }

    fn stream_send(_: u64, _: String) -> Result<(), String> {
        Err("not supported".to_owned())
    }

    fn stream_receive(_: u64) -> Result<String, String> {
        Err("not supported".to_owned())
    }

    fn stream_close_send(_: u64) -> Result<(), String> {
        Err("not supported".to_owned())
    }

    fn stream_cancel(_: u64) {}
}

export!(GuestComponent);
