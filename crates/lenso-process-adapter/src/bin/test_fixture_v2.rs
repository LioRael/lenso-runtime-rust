use std::{collections::BTreeMap, sync::Mutex};

use lenso_process_protocol::authoring::{
    ConstructParams, InitializeParams, InvocationOutcome, InvokeParams, StopParams,
};
use lenso_process_sdk::{
    ProcessInvocationContext, ProcessLifecycleContext, ProcessPluginV2, ProcessStopOutcome,
};
use serde_json::{Value, json};

#[derive(Debug, Default)]
struct SyncPlugin {
    routes: Mutex<BTreeMap<String, String>>,
    lifecycle_calls: Mutex<bool>,
    exit_after_construct_ms: Mutex<Option<u64>>,
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
        *self.lifecycle_calls.lock().expect("fixture lifecycle flag") = params
            .config
            .get("lifecycle_calls")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        *self
            .exit_after_construct_ms
            .lock()
            .expect("fixture exit flag") = params
            .config
            .get("exit_after_construct_ms")
            .and_then(Value::as_u64);
        Ok(())
    }

    fn construct(
        &self,
        _params: &ConstructParams,
        context: ProcessLifecycleContext,
    ) -> Result<Self::Instance, String> {
        let routes = self.routes.lock().expect("fixture routes");
        if *self.lifecycle_calls.lock().expect("fixture lifecycle flag") {
            match context.call(
                "source",
                &routes["source"],
                "read",
                json!({"document": "startup"}),
            ) {
                Ok(InvocationOutcome::Success { .. }) => {}
                Ok(outcome) => return Err(format!("unexpected construction outcome: {outcome:?}")),
                Err(detail) => return Err(detail),
            }
        }
        if let Some(delay) = *self
            .exit_after_construct_ms
            .lock()
            .expect("fixture exit flag")
        {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(delay));
                std::process::exit(23);
            });
        }
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

    fn stop(
        &self,
        _instance: &Self::Instance,
        _params: &StopParams,
        context: ProcessLifecycleContext,
    ) -> ProcessStopOutcome {
        if *self.lifecycle_calls.lock().expect("fixture lifecycle flag") {
            let route = self.routes.lock().expect("fixture routes")["destination"].clone();
            return match context.call(
                "destination",
                route,
                "put",
                json!({"document": "cleanup", "text": "stopped"}),
            ) {
                Ok(InvocationOutcome::Success { .. }) => ProcessStopOutcome::Completed,
                Ok(outcome) => {
                    ProcessStopOutcome::Failed(format!("unexpected cleanup outcome: {outcome:?}"))
                }
                Err(detail) => ProcessStopOutcome::Failed(detail),
            };
        }
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
