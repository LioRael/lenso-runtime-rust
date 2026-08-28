use lenso::{InvocationOutcome, JsonRequestHandler};

#[lenso::plugin]
#[derive(Default)]
struct Echo;

impl JsonRequestHandler for Echo {
    fn invoke(
        &self,
        capability: &str,
        operation: &str,
        request: serde_json::Value,
    ) -> InvocationOutcome {
        if capability != "dev.fixture.echo@1" || operation != "echo" {
            return InvocationOutcome::DomainError(serde_json::json!("not_found"));
        }
        InvocationOutcome::Success(request)
    }
}

lenso::__export_json_request_handler! {
    Echo {
        capability_id: "dev.fixture.echo@1",
        descriptor_version: "1.0.0",
        requests: ["echo"],
    }
}
