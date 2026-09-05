#![cfg(feature = "test-fixture")]

use std::{
    io::{BufRead, BufReader, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
};

use lenso_process_sdk::authoring::{
    AuthoringLimits, ConstructParams, InitializeParams, InvocationOutcome, InvocationResult,
    InvocationScope, InvokeParams, ProvidedEndpoint, RequirementCardinality,
    RequirementDeclaration, RouteDescriptor, SessionIdentity, StopHookOutcome, StopParams,
};
use lenso_process_sdk::{GuestFrameV2, HostFrameV2};
use serde_json::json;

const STORE_DIGEST: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const SYNC_DIGEST: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

#[test]
fn generated_process_entry_constructs_one_object_calls_dependencies_and_stops() {
    let executable = env!("CARGO_BIN_EXE_lenso-plugin-sdk-process-fixture");
    let mut child = Command::new(executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send(&mut stdin, &HostFrameV2::Initialize(initialization()));
    assert!(matches!(receive(&mut stdout), GuestFrameV2::Initialized(_)));

    send(
        &mut stdin,
        &HostFrameV2::Construct(ConstructParams {
            session: "session-1".to_owned(),
            lifecycle_scope_id: "construct-1".to_owned(),
            remaining_budget_nanos: "1000000000".to_owned(),
        }),
    );
    assert!(matches!(receive(&mut stdout), GuestFrameV2::Constructed(_)));

    send(
        &mut stdin,
        &HostFrameV2::Invoke(InvokeParams {
            session: "session-1".to_owned(),
            correlation_id: "1".to_owned(),
            endpoint_id: "sync".to_owned(),
            capability_id: "example.document-sync@1".to_owned(),
            descriptor_version: "1.0.0".to_owned(),
            descriptor_digest: SYNC_DIGEST.to_owned(),
            operation: "sync".to_owned(),
            scope: scope("invoke-1"),
            payload: json!({ "document": "guide" }),
        }),
    );
    let source = outbound(&mut stdout, "source", "read", "invoke-1");
    respond(&mut stdin, &source, json!({ "text": "complete object" }));
    let destination = outbound(&mut stdout, "destination", "put", "invoke-1");
    assert_eq!(destination.payload["text"], "complete object");
    respond(&mut stdin, &destination, json!({ "stored": true }));
    let result = receive(&mut stdout);
    assert!(matches!(
        result,
        GuestFrameV2::InvocationResult(InvocationResult {
            outcome: InvocationOutcome::Success { value },
            ..
        }) if value == json!({ "document": "guide", "text": "complete object" })
    ));
    assert!(matches!(receive(&mut stdout), GuestFrameV2::Settlement(_)));

    send(
        &mut stdin,
        &HostFrameV2::Stop(StopParams {
            session: "session-1".to_owned(),
            cleanup_scope_id: "cleanup-1".to_owned(),
            remaining_budget_nanos: "1000000000".to_owned(),
        }),
    );
    let cleanup = outbound(&mut stdout, "destination", "put", "cleanup-1");
    assert_eq!(cleanup.payload["document"], "cleanup");
    respond(&mut stdin, &cleanup, json!({ "stored": true }));
    assert!(matches!(
        receive(&mut stdout),
        GuestFrameV2::Stopped(result) if result.hook == StopHookOutcome::Completed
    ));
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

fn initialization() -> InitializeParams {
    InitializeParams {
        api_version: 2,
        identity: SessionIdentity {
            session: "session-1".to_owned(),
            plugin_instance: "sync".to_owned(),
            plugin_generation: "1".to_owned(),
            artifact_digest: STORE_DIGEST.to_owned(),
            contract_digest: SYNC_DIGEST.to_owned(),
            runtime_profile: "lenso.process-stdio@2".to_owned(),
            value_profile: "lenso-json-value-v1".to_owned(),
        },
        config: json!({}),
        required_declarations: ["destination", "source"]
            .into_iter()
            .map(|requirement_id| RequirementDeclaration {
                requirement_id: requirement_id.to_owned(),
                capability_id: "example.document-store@1".to_owned(),
                descriptor_version: "1.0.0".to_owned(),
                descriptor_digest: STORE_DIGEST.to_owned(),
                cardinality: RequirementCardinality::One,
            })
            .collect(),
        routes: ["destination", "source"]
            .into_iter()
            .enumerate()
            .map(|(index, requirement_id)| RouteDescriptor {
                route_id: format!("route-{requirement_id}"),
                requirement_id: requirement_id.to_owned(),
                capability_id: "example.document-store@1".to_owned(),
                descriptor_version: "1.0.0".to_owned(),
                descriptor_digest: STORE_DIGEST.to_owned(),
                provider_instance: format!("store-{index}"),
                provider_order: 0,
            })
            .collect(),
        provided_endpoints: vec![ProvidedEndpoint {
            endpoint_id: "sync".to_owned(),
            capability_id: "example.document-sync@1".to_owned(),
            descriptor_version: "1.0.0".to_owned(),
            descriptor_digest: SYNC_DIGEST.to_owned(),
        }],
        limits: AuthoringLimits::defaults(),
    }
}

fn scope(id: &str) -> InvocationScope {
    InvocationScope {
        scope_id: id.to_owned(),
        parent_scope_id: None,
        remaining_budget_nanos: "1000000000".to_owned(),
        permissions: Vec::new(),
        extensions: Vec::new(),
    }
}

fn send(stdin: &mut ChildStdin, frame: &HostFrameV2) {
    serde_json::to_writer(&mut *stdin, &frame).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
}

fn receive(stdout: &mut BufReader<ChildStdout>) -> GuestFrameV2 {
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(!line.is_empty(), "Process closed before returning a frame");
    serde_json::from_str(&line).unwrap()
}

fn outbound(
    stdout: &mut BufReader<ChildStdout>,
    requirement: &str,
    operation: &str,
    parent_scope: &str,
) -> lenso_process_sdk::authoring::OutboundCallParams {
    match receive(stdout) {
        GuestFrameV2::OutboundCall(call) => {
            assert_eq!(call.requirement_id, requirement);
            assert_eq!(call.operation, operation);
            assert_eq!(call.scope.parent_scope_id.as_deref(), Some(parent_scope));
            call
        }
        other => panic!("expected outbound call, got {other:?}"),
    }
}

fn respond(
    stdin: &mut ChildStdin,
    call: &lenso_process_sdk::authoring::OutboundCallParams,
    value: serde_json::Value,
) {
    send(
        stdin,
        &HostFrameV2::OutboundResult(InvocationResult {
            session: call.session.clone(),
            correlation_id: call.correlation_id.clone(),
            outcome: InvocationOutcome::Success { value },
        }),
    );
}
