wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

lenso_guest_sdk::wasm_host! {
    struct WasmHost {
        bindings: host_bindings,
        invoke: host_invoke,
        stream_open: host_stream_open,
        stream_send: host_stream_send,
        stream_receive: host_stream_receive,
        stream_close_send: host_stream_close_send,
        stream_cancel: host_stream_cancel,
    }
}

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
        let context = lenso_guest_sdk::GuestContext::load(WasmHost).unwrap();
        let probe = context
            .require(
                "lenso.runtime.conformance.probe@1",
                "1.0.0",
                &["probe"],
                &[],
            )
            .unwrap();
        assert_eq!(probe.binding().provider_instance(), "provider");
        let request: serde_json::Value = serde_json::from_str(&request_json).unwrap();
        let imported = probe
            .request::<_, serde_json::Value, serde_json::Value>(
                "probe",
                &serde_json::json!({ "value": format!("wasm-{request}") }),
            )
            .unwrap();
        assert_eq!(imported["value"], format!("Echo: wasm-{request}"));
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
