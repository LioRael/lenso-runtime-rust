wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

mod echo {
    pub const CAPABILITY_ID: &str = "test.echo@1";
    pub const DESCRIPTOR_VERSION: &str = "1.0.0";
    pub const ECHO: &str = "echo";
    pub const FAIL: &str = "fail";
    pub const TRAP: &str = "trap";
    pub const LOOP: &str = "loop";
}

struct GuestComponent;

impl Guest for GuestComponent {
    fn describe() -> String {
        lenso_guest_sdk::guest_descriptor! {
            provides: [echo {
                requests: [echo::ECHO, echo::FAIL, echo::TRAP, echo::LOOP],
                streams: [],
            }],
            requires: [],
        }
    }

    fn invoke(
        capability: String,
        operation: String,
        request_json: String,
    ) -> Result<String, String> {
        assert_eq!(capability, "test.echo@1");
        match operation.as_str() {
            "fail" => Err("\"declared\"".to_owned()),
            "trap" => panic!("guest trap"),
            "loop" => loop {
                std::hint::spin_loop();
            },
            _ => Ok(request_json),
        }
    }
}

export!(GuestComponent);
