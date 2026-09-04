use std::{fs, path::Path};

use lenso_app_plan::{ExecutionClassId, PluginInstancePlan, ResolvedAppPlan};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::VerifiedDistribution;

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn platform() -> (&'static str, &'static str, &'static str) {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ("aarch64-apple-darwin", "darwin", "arm64"),
        ("linux", "x86_64") => ("x86_64-unknown-linux-gnu", "linux", "x64"),
        values => panic!("unsupported test platform: {values:?}"),
    }
}

fn write_executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn verifies_every_locked_file_and_rejects_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::create_dir_all(root.join(".lenso")).unwrap();
    fs::create_dir(root.join("runtime")).unwrap();
    fs::write(root.join(".lenso/host-build.json"), b"{}\n").unwrap();
    fs::write(root.join("bundles.json"), b"[]\n").unwrap();
    write_executable(root.join("host.js").as_path(), b"entrypoint\n");
    fs::write(root.join("THIRD_PARTY_NOTICES.txt"), b"notices\n").unwrap();
    write_executable(
        root.join("runtime/lenso-host-runtime").as_path(),
        b"runtime\n",
    );
    write_executable(root.join("runtime/lenso-resolver").as_path(), b"resolver\n");
    write_executable(
        root.join("runtime/lenso-process-owner").as_path(),
        b"owner\n",
    );
    write_executable(root.join("runtime/bun").as_path(), b"bun\n");
    let files = [
        (".lenso/host-build.json", "host_authority", false),
        ("bundles.json", "bundle_inventory", false),
        ("runtime/lenso-host-runtime", "host_runtime", true),
        ("runtime/lenso-resolver", "runtime_resolver", true),
        ("runtime/lenso-process-owner", "process_owner", true),
        ("runtime/bun", "javascript_runtime", true),
        ("host.js", "entrypoint", true),
        ("THIRD_PARTY_NOTICES.txt", "notices", false),
    ]
    .into_iter()
    .map(|(path, role, executable)| {
        let bytes = fs::read(root.join(path)).unwrap();
        json!({
            "path": path,
            "role": role,
            "sha256": digest(&bytes),
            "size": bytes.len(),
            "executable": executable,
        })
    })
    .collect::<Vec<_>>();
    let (target, platform, arch) = platform();
    fs::write(
        root.join(".lenso/distribution.lock.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": "lenso.host-distribution.v1",
            "app_id": "company.app",
            "target": target,
            "platform": platform,
            "arch": arch,
            "files": files,
        }))
        .unwrap(),
    )
    .unwrap();

    let distribution = VerifiedDistribution::open(root).unwrap();
    assert_eq!(distribution.app_id(), "company.app");
    assert!(distribution.identity().starts_with("sha256:"));

    fs::write(root.join("bundles.json"), b"tampered\n").unwrap();
    let error = VerifiedDistribution::open(root).unwrap_err().to_string();
    assert!(error.contains("differs from lock") || error.contains("failed integrity"));
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one complete prepared-distribution journey"
)]
fn resolves_external_root_and_closes_instance_artifact() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("distribution");
    let app = temporary.path().join("app");
    fs::create_dir_all(root.join(".lenso")).unwrap();
    fs::create_dir_all(root.join("runtime")).unwrap();
    fs::create_dir_all(root.join("bundles")).unwrap();
    fs::create_dir_all(root.join("artifacts/0")).unwrap();
    fs::create_dir(&app).unwrap();

    let host_bytes = b"{\"host\":\"fixture\"}\n";
    let bundle_bytes = b"fixture bundle\n";
    let artifact_bytes = b"export default {};\n";
    fs::write(root.join(".lenso/host-build.json"), host_bytes).unwrap();
    fs::write(root.join("bundles/0.lenso-plugin"), bundle_bytes).unwrap();
    fs::write(root.join("artifacts/0/plugin.js"), artifact_bytes).unwrap();
    write_executable(root.join("host.js").as_path(), b"entrypoint\n");
    fs::write(root.join("THIRD_PARTY_NOTICES.txt"), b"notices\n").unwrap();
    write_executable(
        root.join("runtime/lenso-host-runtime").as_path(),
        b"runtime\n",
    );

    let plan = ResolvedAppPlan::new(
        vec![
            PluginInstancePlan::new("plugin", "company.plugin")
                .with_entrypoint("plugin.js")
                .with_execution_class(ExecutionClassId::bun_child_process())
                .with_package_revision("1.0.0"),
        ],
        vec![],
    );
    plan.validate().unwrap();
    let resolution = serde_json::to_string(&json!({
        "schema": "lenso.runtime-app-resolution.v1",
        "app_id": "company.app",
        "authority_digest": digest(b"authority"),
        "host_build_digest": digest(host_bytes),
        "plugin_root_revision": digest(b"root"),
        "plan": plan,
    }))
    .unwrap();
    let resolver = format!("#!/bin/sh\nprintf '%s\\n' '{resolution}'\n");
    write_executable(
        root.join("runtime/lenso-resolver").as_path(),
        resolver.as_bytes(),
    );
    write_executable(
        root.join("runtime/lenso-process-owner").as_path(),
        b"owner\n",
    );
    write_executable(root.join("runtime/bun").as_path(), b"bun\n");
    let manifest_digest = digest(b"manifest");
    fs::write(
        root.join("bundles.json"),
        serde_json::to_vec_pretty(&json!([{
            "path": "bundles/0.lenso-plugin",
            "plugin_id": "company.plugin",
            "release_version": "1.0.0",
            "manifest_digest": manifest_digest,
            "execution_class": "lenso.bun-process@1",
            "target": platform().0,
            "implementation_id": "bun",
            "artifact_path": "implementations/bun/plugin.js",
            "artifact_digest": digest(artifact_bytes),
            "artifact_size": artifact_bytes.len(),
            "artifact_media_type": "application/javascript",
            "artifact_target": "javascript-bun"
        }]))
        .unwrap(),
    )
    .unwrap();

    let locked = [
        (".lenso/host-build.json", "host_authority", false),
        ("bundles.json", "bundle_inventory", false),
        ("bundles/0.lenso-plugin", "plugin_bundle", false),
        ("artifacts/0/plugin.js", "plugin_artifact", false),
        ("runtime/lenso-host-runtime", "host_runtime", true),
        ("runtime/lenso-resolver", "runtime_resolver", true),
        ("runtime/lenso-process-owner", "process_owner", true),
        ("runtime/bun", "javascript_runtime", true),
        ("host.js", "entrypoint", true),
        ("THIRD_PARTY_NOTICES.txt", "notices", false),
    ]
    .into_iter()
    .map(|(path, role, executable)| {
        let bytes = fs::read(root.join(path)).unwrap();
        json!({
            "path": path,
            "role": role,
            "sha256": digest(&bytes),
            "size": bytes.len(),
            "executable": executable,
        })
    })
    .collect::<Vec<_>>();
    let (target, platform, arch) = platform();
    fs::write(
        root.join(".lenso/distribution.lock.json"),
        serde_json::to_vec_pretty(&json!({
            "schema": "lenso.host-distribution.v1",
            "app_id": "company.app",
            "target": target,
            "platform": platform,
            "arch": arch,
            "files": locked,
        }))
        .unwrap(),
    )
    .unwrap();

    let prepared = VerifiedDistribution::open(&root)
        .unwrap()
        .resolve(&app)
        .unwrap();
    assert_eq!(prepared.generation.plan.plugin_instances().len(), 1);
    let artifact = prepared.generation.artifacts.require("plugin").unwrap();
    assert_eq!(fs::read(artifact.path()).unwrap(), artifact_bytes);
    assert_ne!(artifact.path(), root.join("artifacts/0/plugin.js"));
}
