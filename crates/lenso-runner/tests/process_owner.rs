#![cfg(all(
    feature = "process-owner",
    any(target_os = "macos", target_os = "linux")
))]

use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn write_frame(writer: &mut impl Write, value: &Value) {
    let bytes = serde_json::to_vec(value).unwrap();
    writer
        .write_all(&u32::try_from(bytes.len()).unwrap().to_be_bytes())
        .unwrap();
    writer.write_all(&bytes).unwrap();
    writer.flush().unwrap();
}

fn start(root: &Path, registry: &Path) -> Value {
    json!({"version":1,"distribution":"fixture-v1","request_id":1,"root":root,"registry":registry,
        "executable":"/bin/sh","arguments":["-c", "trap '' TERM; sleep 60 & echo $! > child.pid; wait"],
        "stop_ms":50,"confirmation_ms":2000})
}

fn owner(input: &Value) -> (Child, mpsc::Receiver<Value>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_lenso-process-owner"))
        .arg("fixture-v1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut output = child.stdout.take().unwrap();
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        loop {
            let mut header = [0; 4];
            if output.read_exact(&mut header).is_err() {
                break;
            }
            let length = u32::from_be_bytes(header) as usize;
            assert!(length <= 256 * 1024);
            let mut bytes = vec![0; length];
            output.read_exact(&mut bytes).unwrap();
            if send.send(serde_json::from_slice(&bytes).unwrap()).is_err() {
                break;
            }
        }
    });
    write_frame(child.stdin.as_mut().unwrap(), input);
    (child, receive)
}

fn event(events: &mpsc::Receiver<Value>) -> Value {
    events
        .recv_timeout(Duration::from_secs(5))
        .expect("bounded owner response")
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "bounded process cleanup");
        thread::sleep(Duration::from_millis(10));
    }
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("app");
    fs::create_dir(&root).unwrap();
    let registry = temp.path().join("owners");
    (temp, root, registry)
}

fn confirm(child: &mut Child, events: &mpsc::Receiver<Value>, pid: Pid) {
    let terminal = event(events);
    assert_eq!(terminal["kind"], "terminal");
    assert_eq!(terminal["termination"], "confirmed", "{terminal}");
    assert_eq!(terminal["forced"], true);
    wait_for(|| child.try_wait().unwrap().is_some());
    assert_eq!(test_kill_process_group(pid), Err(rustix::io::Errno::SRCH));
}

#[test]
fn owner_stop_eof_runtime_death_and_invalid_frame_settle_real_descendants() {
    for mode in ["stop", "eof", "runtime_death", "invalid"] {
        let (_temp, root, registry) = fixture();
        let input = start(&root, &registry);
        let (mut child, events) = owner(&input);
        let owned = event(&events);
        assert_eq!(owned["kind"], "owned");
        let pid = Pid::from_raw(i32::try_from(owned["pid"].as_u64().unwrap()).unwrap()).unwrap();
        wait_for(|| root.join("child.pid").exists());
        match mode {
            "stop" => {
                write_frame(
                    child.stdin.as_mut().unwrap(),
                    &json!({"version":1,"request_id":2,"op":"stop"}),
                );
                write_frame(
                    child.stdin.as_mut().unwrap(),
                    &json!({"version":1,"request_id":3,"op":"stop"}),
                );
            }
            "eof" => {
                drop(child.stdin.take());
            }
            "runtime_death" => rustix::process::kill_process(pid, Signal::KILL).unwrap(),
            "invalid" => child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(&u32::MAX.to_be_bytes())
                .unwrap(),
            _ => unreachable!(),
        }
        confirm(&mut child, &events, pid);
        // A confirmed group exit, not a PID file or elapsed deadline, admits the next owner.
        let (mut next, replies) = owner(&input);
        let next_pid =
            Pid::from_raw(i32::try_from(event(&replies)["pid"].as_u64().unwrap()).unwrap())
                .unwrap();
        drop(next.stdin.take());
        confirm(&mut next, &replies, next_pid);
    }
}

#[test]
fn incompatible_start_never_executes_the_supplied_program() {
    let (_temp, root, registry) = fixture();
    for (field, value) in [
        ("version", json!(2)),
        ("distribution", json!("different")),
        ("stop_ms", json!(0)),
        ("request_id", json!(2)),
    ] {
        let mut input = start(&root, &registry);
        input["arguments"] = json!(["-c", "touch should-not-run"]);
        input[field] = value;
        let (mut rejected, _) = owner(&input);
        wait_for(|| rejected.try_wait().unwrap().is_some());
        assert!(!rejected.wait().unwrap().success());
        assert!(!root.join("should-not-run").exists());
    }
}

#[test]
fn root_alias_rename_and_replacement_cannot_bypass_ownership() {
    let (temp, root, registry) = fixture();
    let (mut child, events) = owner(&start(&root, &registry));
    let pid =
        Pid::from_raw(i32::try_from(event(&events)["pid"].as_u64().unwrap()).unwrap()).unwrap();
    let alias = temp.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let moved = temp.path().join("moved");
    for path in [&alias, &moved, &root] {
        if path == &moved {
            fs::rename(&root, &moved).unwrap();
            fs::create_dir(&root).unwrap();
        }
        let (mut rejected, _) = owner(&start(path, &registry));
        wait_for(|| rejected.try_wait().unwrap().is_some());
        assert!(!rejected.wait().unwrap().success());
    }
    drop(child.stdin.take());
    confirm(&mut child, &events, pid);
}

#[test]
fn owner_death_leaves_uncertainty_that_blocks_automatic_recovery() {
    let (_temp, root, registry) = fixture();
    let input = start(&root, &registry);
    let (mut child, events) = owner(&input);
    let pid =
        Pid::from_raw(i32::try_from(event(&events)["pid"].as_u64().unwrap()).unwrap()).unwrap();
    wait_for(|| root.join("child.pid").exists());
    child.kill().unwrap();
    child.wait().unwrap();
    let (mut rejected, _) = owner(&input);
    wait_for(|| rejected.try_wait().unwrap().is_some());
    assert!(!rejected.wait().unwrap().success());
    // Test fixture owns this still-live, non-detaching group. Never clear its
    // uncertainty marker based on a guessed PID or turn the check into recovery.
    kill_process_group(pid, Signal::KILL).unwrap();
    wait_for(|| test_kill_process_group(pid) == Err(rustix::io::Errno::SRCH));
}

#[test]
fn killed_launcher_cannot_leave_runtime_or_descendant_running() {
    let (temp, root, registry) = fixture();
    let mut launcher = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "launcher_fixture", "--nocapture"])
        .env("LENSO_OWNER_TEST_DIRECTORY", temp.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(|| temp.path().join("owned.json").exists() && root.join("child.pid").exists());
    let owned: Value =
        serde_json::from_slice(&fs::read(temp.path().join("owned.json")).unwrap()).unwrap();
    let pid = Pid::from_raw(i32::try_from(owned["pid"].as_u64().unwrap()).unwrap()).unwrap();
    launcher.kill().unwrap();
    launcher.wait().unwrap();
    wait_for(|| test_kill_process_group(pid) == Err(rustix::io::Errno::SRCH));
    wait_for(|| {
        fs::read_dir(&registry)
            .unwrap()
            .all(|entry| fs::read(entry.unwrap().path()).unwrap() == b"settled\n")
    });
}

#[test]
fn launcher_fixture() {
    let Some(directory) = std::env::var_os("LENSO_OWNER_TEST_DIRECTORY") else {
        return;
    };
    let directory = Path::new(&directory);
    let (_child, events) = owner(&start(&directory.join("app"), &directory.join("owners")));
    fs::write(
        directory.join("owned.tmp"),
        serde_json::to_vec(&event(&events)).unwrap(),
    )
    .unwrap();
    fs::rename(directory.join("owned.tmp"), directory.join("owned.json")).unwrap();
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}
