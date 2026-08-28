use lenso_app_plan::{ExecutionClassId, authoring::PluginDescriptor};

use crate::{
    BundleError, PluginArtifactV2, PluginImplementationV3, PluginManifest, invalid_bundle,
};

/// Host policy used to resolve one implementation before Plan construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImplementationPolicy {
    pub host_target: String,
    pub execution_classes: Vec<ExecutionClassId>,
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
            if !policy
                .execution_classes
                .contains(descriptor.execution_class())
                || !target_matches(&value.artifact, &policy.host_target)
            {
                return invalid_bundle("V2 Bundle has no implementation admitted by Host policy");
            }
            Ok(ResolvedPluginImplementation {
                implementation_id: "default".to_owned(),
                descriptor,
                artifact: value.artifact.clone(),
            })
        }
        PluginManifest::V3(value) => {
            for execution_class in &policy.execution_classes {
                let matches = value
                    .implementations
                    .iter()
                    .filter(|candidate| {
                        candidate.runtime.execution_class() == execution_class
                            && candidate
                                .host_targets
                                .iter()
                                .any(|target| target == "*" || target == &policy.host_target)
                    })
                    .collect::<Vec<_>>();
                match matches.as_slice() {
                    [] => {}
                    [candidate] => return Ok(resolved_v3(value, candidate)),
                    _ => {
                        return invalid_bundle(format!(
                            "Host policy ambiguously matches {} implementations of `{}`",
                            matches.len(),
                            execution_class.as_str()
                        ));
                    }
                }
            }
            invalid_bundle("V3 Bundle has no implementation admitted by Host policy")
        }
    }
}

fn resolved_v3(
    manifest: &crate::PluginManifestV3,
    candidate: &PluginImplementationV3,
) -> ResolvedPluginImplementation {
    ResolvedPluginImplementation {
        implementation_id: candidate.id.clone(),
        descriptor: manifest.contract.resolve(&candidate.runtime),
        artifact: candidate.artifact.clone(),
    }
}

fn target_matches(artifact: &PluginArtifactV2, host_target: &str) -> bool {
    artifact.media_type == "application/wasm" || artifact.target == host_target
}
