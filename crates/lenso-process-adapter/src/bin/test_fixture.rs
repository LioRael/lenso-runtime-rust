use lenso_process_sdk::{ProcessOutcome, ProcessPlugin};
use serde_json::{Value, json};

#[derive(Debug)]
struct Echo;

impl ProcessPlugin for Echo {
    fn descriptor(&self) -> Value {
        json!({
            "abi": "lenso.json-request@1",
            "capabilities": [{
                "capability_id": "example.echo@1",
                "descriptor_version": "1.0.0",
                "request_operations": ["echo"],
            }],
        })
    }

    fn invoke(&self, capability: &str, operation: &str, request: Value) -> ProcessOutcome {
        if capability == "example.echo@1" && operation == "echo" {
            if request.get("unicode_failure") == Some(&Value::Bool(true)) {
                return ProcessOutcome::Failure("界".repeat(200));
            }
            if let Some(milliseconds) = request.get("sleep_ms").and_then(Value::as_u64) {
                std::thread::sleep(std::time::Duration::from_millis(milliseconds));
            }
            ProcessOutcome::Success(request)
        } else {
            ProcessOutcome::DomainError(json!("not_found"))
        }
    }
}

fn main() {
    lenso_process_sdk::serve(&Echo).expect("serve Process Plugin fixture");
}
