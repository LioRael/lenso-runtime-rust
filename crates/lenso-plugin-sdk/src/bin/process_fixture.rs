use lenso_plugin_sdk::{
    CallError, CreateContext, Ctx, Dependency, DependencyClient, InvocationOutcome,
    JsonRequestHandler, Plugin, Requirement,
};
use serde_json::{Value, json};

const STORE_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";

#[derive(Debug)]
struct DocumentStoreClient {
    dependency: Dependency,
}

impl DependencyClient for DocumentStoreClient {
    const CAPABILITY_ID: &'static str = "example.document-store@1";
    const DESCRIPTOR_VERSION: &'static str = "1.0.0";
    const DESCRIPTOR_DIGEST: &'static str = STORE_DIGEST;

    fn from_dependency(dependency: Dependency) -> Self {
        Self { dependency }
    }
}

impl DocumentStoreClient {
    fn request<Response>(
        &self,
        context: &Ctx,
        operation: &str,
        request: &Value,
    ) -> Result<Response, CallError<Value>>
    where
        Response: serde::de::DeserializeOwned,
    {
        context.request(&self.dependency, operation, request)
    }
}

const REQUIREMENTS: &[Requirement] = &[
    Requirement::one::<DocumentStoreClient>("destination"),
    Requirement::one::<DocumentStoreClient>("source"),
];

#[lenso_plugin_sdk::plugin]
struct SyncPlugin {
    source: DocumentStoreClient,
    destination: DocumentStoreClient,
}

impl Plugin for SyncPlugin {
    fn requirements() -> &'static [Requirement] {
        REQUIREMENTS
    }

    fn create(context: CreateContext) -> Result<Self, String> {
        Ok(Self {
            source: context.dependencies().one("source")?.client()?,
            destination: context.dependencies().one("destination")?.client()?,
        })
    }

    fn stop(&self, context: Ctx) -> Result<(), String> {
        self.destination
            .request::<Value>(
                &context,
                "put",
                &json!({ "document": "cleanup", "text": "stopped" }),
            )
            .map(|_| ())
            .map_err(|error| format!("cleanup failed: {error:?}"))
    }
}

impl JsonRequestHandler for SyncPlugin {
    fn invoke_with_context(
        &self,
        context: Ctx,
        capability: &str,
        operation: &str,
        request: Value,
    ) -> InvocationOutcome {
        if capability != "example.document-sync@1" || operation != "sync" {
            return InvocationOutcome::DomainError(json!("not_found"));
        }
        let document = request
            .get("document")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let source =
            match self
                .source
                .request::<Value>(&context, "read", &json!({ "document": document }))
            {
                Ok(value) => value,
                Err(error) => return InvocationOutcome::Failure(format!("read failed: {error:?}")),
            };
        let text = source
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Err(error) = self.destination.request::<Value>(
            &context,
            "put",
            &json!({ "document": document, "text": text }),
        ) {
            return InvocationOutcome::Failure(format!("write failed: {error:?}"));
        }
        InvocationOutcome::Success(json!({ "document": document, "text": text }))
    }
}

lenso_plugin_sdk::__export_json_request_handler! {
    SyncPlugin {
        capability_id: "example.document-sync@1",
        descriptor_version: "1.0.0",
        descriptor_digest: "sha256:2222222222222222222222222222222222222222222222222222222222222222",
        requests: ["sync"],
    }
}
