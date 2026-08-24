wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

struct GuestComponent;

impl Guest for GuestComponent {
    fn invoke(operation: String, request_json: String) -> Result<String, String> {
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
