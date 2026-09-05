use std::{collections::BTreeMap, sync::Mutex};

use lenso_process_protocol::authoring::{
    ConstructParams, InitializeParams, InvocationOutcome, InvokeParams, StopParams,
};
use lenso_process_sdk::{ProcessInvocationContext, ProcessPluginV2, ProcessStopOutcome};
use serde_json::{Value, json};

#[derive(Debug, Default)]
struct SyncPlugin {
    routes: Mutex<BTreeMap<String, String>>,
}

#[derive(Debug)]
struct SyncInstance {
    source_route: String,
    destination_route: String,
}

impl ProcessPluginV2 for SyncPlugin {
    type Instance = SyncInstance;

    fn initialize(&self, params: &InitializeParams) -> Result<(), String> {
        let routes = params
            .routes
            .iter()
            .map(|route| (route.requirement_id.clone(), route.route_id.clone()))
            .collect::<BTreeMap<_, _>>();
        if routes.len() != 2
            || !routes.contains_key("source")
            || !routes.contains_key("destination")
        {
            return Err("expected exact source and destination Store routes".to_owned());
        }
        *self.routes.lock().expect("fixture routes") = routes;
        Ok(())
    }

    fn construct(&self, _params: &ConstructParams) -> Result<Self::Instance, String> {
        let routes = self.routes.lock().expect("fixture routes");
        Ok(SyncInstance {
            source_route: routes["source"].clone(),
            destination_route: routes["destination"].clone(),
        })
    }

    fn invoke(
        &self,
        instance: &Self::Instance,
        params: InvokeParams,
        context: ProcessInvocationContext,
    ) -> InvocationOutcome {
        if params.capability_id != "example.sync@1" || params.operation != "sync" {
            return InvocationOutcome::Domain {
                error: json!({"kind": "unknown_operation"}),
            };
        }
        if let Some(milliseconds) = params.payload.get("sleep_ms").and_then(Value::as_u64) {
            std::thread::sleep(std::time::Duration::from_millis(milliseconds));
        }
        let document = params
            .payload
            .get("document")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let source_route = if params
            .payload
            .get("forge_route")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "forged-route"
        } else {
            &instance.source_route
        };
        let source = match context.call(
            "source",
            source_route,
            "read",
            json!({"document": document}),
        ) {
            Ok(InvocationOutcome::Success { value }) => value,
            Ok(outcome) => return outcome,
            Err(detail) => return runtime_failure(detail),
        };
        let text = source
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match context.call(
            "destination",
            &instance.destination_route,
            "put",
            json!({"document": document, "text": text}),
        ) {
            Ok(InvocationOutcome::Success { .. }) => InvocationOutcome::Success {
                value: json!({"document": document, "text": text}),
            },
            Ok(outcome) => outcome,
            Err(detail) => runtime_failure(detail),
        }
    }

    fn stop(&self, _instance: &Self::Instance, _params: &StopParams) -> ProcessStopOutcome {
        ProcessStopOutcome::Completed
    }
}

fn runtime_failure(detail: String) -> InvocationOutcome {
    InvocationOutcome::Runtime {
        failure: lenso_process_protocol::authoring::RuntimeFailure::PluginFailure { detail },
    }
}

fn main() {
    lenso_process_sdk::serve_v2(SyncPlugin::default()).expect("serve Process V2 Plugin fixture");
}
