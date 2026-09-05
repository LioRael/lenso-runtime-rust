use lenso_app_plan::{ExecutionClassId, authoring::PluginDescriptor};

use crate::{BundleError, PluginArtifactV2, PluginManifest, invalid_bundle};

/// One exact runtime protocol admitted by the Host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeAdmission {
    pub execution_class: ExecutionClassId,
    pub runtime_profile: String,
}

/// Host policy used to resolve one implementation before Plan construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImplementationPolicy {
    pub host_target: String,
    pub runtimes: Vec<RuntimeAdmission>,
}

/// One final implementation selected from a Plugin Release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPluginImplementation {
    pub implementation_id: String,
    pub descriptor: PluginDescriptor,
    pub artifact: PluginArtifactV2,
}

/// Selects one implementation deterministically. Selection never implies runtime fallback.
pub fn resolve_implementation(
    manifest: &PluginManifest,
    policy: &ImplementationPolicy,
) -> Result<ResolvedPluginImplementation, BundleError> {
    match manifest {
        PluginManifest::V2(value) => {
            let descriptor =
                serde_json::from_value::<PluginDescriptor>(value.entry.descriptor.clone())
                    .map_err(|error| BundleError::InvalidManifest(error.to_string()))?;
            if !policy.runtimes.iter().any(|admission| {
                admission.execution_class == *descriptor.execution_class()
                    && admission.runtime_profile == descriptor.runtime_profile()
            }) || !target_matches(&value.artifact, &policy.host_target)
            {
                return invalid_bundle("V2 Bundle has no implementation admitted by Host policy");
            }
            Ok(ResolvedPluginImplementation {
                implementation_id: "default".to_owned(),
                descriptor,
                artifact: value.artifact.clone(),
            })
        }
        PluginManifest::V3(value) => resolve_profiled_implementation(
            &value.contract,
            value.implementations.iter().map(|candidate| {
                (
                    &candidate.id,
                    &candidate.host_targets,
                    &candidate.artifact,
                    &candidate.runtime,
                )
            }),
            policy,
            "V3",
        ),
        PluginManifest::V4(value) => resolve_profiled_implementation(
            &value.contract,
            value.implementations.iter().map(|candidate| {
                (
                    &candidate.id,
                    &candidate.host_targets,
                    &candidate.artifact,
                    &candidate.runtime,
                )
            }),
            policy,
            "V4",
        ),
    }
}

fn resolve_profiled_implementation<'a>(
    contract: &lenso_app_plan::authoring::PluginContract,
    candidates: impl Iterator<
        Item = (
            &'a String,
            &'a Vec<String>,
            &'a PluginArtifactV2,
            &'a lenso_app_plan::authoring::PluginImplementation,
        ),
    >,
    policy: &ImplementationPolicy,
    schema: &str,
) -> Result<ResolvedPluginImplementation, BundleError> {
    let candidates = candidates.collect::<Vec<_>>();
    for admission in &policy.runtimes {
        let matches = candidates
            .iter()
            .filter(|(_, targets, _, runtime)| {
                runtime.execution_class() == &admission.execution_class
                    && runtime.runtime_profile() == admission.runtime_profile
                    && targets
                        .iter()
                        .any(|target| target == "*" || target == &policy.host_target)
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => {}
            [(id, _, artifact, runtime)] => {
                return Ok(ResolvedPluginImplementation {
                    implementation_id: (*id).clone(),
                    descriptor: contract.resolve(runtime),
                    artifact: (*artifact).clone(),
                });
            }
            _ => {
                return invalid_bundle(format!(
                    "Host policy ambiguously matches {} implementations of `({}, {})`",
                    matches.len(),
                    admission.execution_class.as_str(),
                    admission.runtime_profile
                ));
            }
        }
    }
    invalid_bundle(format!(
        "{schema} Bundle has no implementation admitted by Host policy"
    ))
}

fn target_matches(artifact: &PluginArtifactV2, host_target: &str) -> bool {
    artifact.media_type == "application/wasm" || artifact.target == host_target
}
