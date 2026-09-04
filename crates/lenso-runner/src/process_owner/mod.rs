use std::{
    io,
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use rustix::{
    io::Errno,
    process::{
        Pid, Signal, WaitId, WaitIdOptions, kill_process_group, test_kill_process_group, waitid,
    },
};

mod guard;
mod relay;
mod wire;

pub fn run() -> io::Result<()> {
    let start = read_start()?;
    #[cfg(target_os = "linux")]
    rustix::process::set_child_subreaper(Pid::from_raw(1)).map_err(io::Error::other)?;
    let mut guard = guard::RootGuard::acquire(&start.root, &start.registry)?;
    let mut child = match Command::new(&start.executable)
        .args(&start.arguments)
        .current_dir(&start.root)
        .stdin(if start.application {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if start.application {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            guard.settled()?;
            return Err(error);
        }
    };
    let pid = Pid::from_raw(i32::try_from(child.id()).map_err(io::Error::other)?)
        .ok_or_else(|| io::Error::other("invalid child PID"))?;
    let stopping = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let invalid = Arc::new(AtomicBool::new(false));
    let (events, write_complete, relay) =
        control_io(stopping.clone(), lost.clone(), invalid.clone(), &mut child);
    let _ = events.try_send(wire::Event::Owned {
        version: wire::VERSION,
        distribution: start.distribution,
        request_id: 1,
        pid: child.id(),
    });
    let mut deadline = None;
    let mut cause = "runtime_exit";
    loop {
        match exited(pid) {
            Ok(true) => break,
            Ok(false) => {}
            Err(_) => {
                cause = "observation_failed";
                break;
            }
        }
        if deadline.is_none()
            && (stopping.load(Ordering::Acquire)
                || lost.load(Ordering::Acquire)
                || invalid.load(Ordering::Acquire))
        {
            cause = if invalid.load(Ordering::Acquire) {
                "invalid_control"
            } else if stopping.load(Ordering::Acquire) {
                "stop_requested"
            } else {
                "launcher_lost"
            };
            deadline = Some(Instant::now() + Duration::from_millis(u64::from(start.stop_ms)));
            // The unreaped direct child pins this PGID until final group cleanup.
            if let Some(relay) = &relay {
                relay.stop(None);
            } else {
                let _ = kill_process_group(pid, Signal::TERM);
            }
        }
        if deadline.is_some_and(|end| Instant::now() >= end) {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    finish(
        Finish {
            child,
            pid,
            guard,
            relay,
            events,
            write_complete,
        },
        cause,
        start.confirmation_ms,
    )
}

#[derive(Debug)]
struct Finish {
    child: Child,
    pid: Pid,
    guard: guard::RootGuard,
    relay: Option<relay::Relay>,
    events: mpsc::SyncSender<wire::Event>,
    write_complete: mpsc::Receiver<()>,
}

fn finish(resources: Finish, mut cause: &'static str, confirmation_ms: u32) -> io::Result<()> {
    let Finish {
        mut child,
        pid,
        mut guard,
        relay,
        events,
        write_complete,
    } = resources;
    let settlement = if cause == "observation_failed" {
        // Lost child-wait authority is not permission to signal a possibly reused PGID.
        Err("observation_failed")
    } else {
        settle(
            &mut child,
            pid,
            Duration::from_millis(u64::from(confirmation_ms)),
        )
    };
    let confirmed = settlement.is_ok();
    if let Err(failure) = settlement {
        cause = failure;
    }
    if confirmed {
        guard.settled()?;
    }
    // A delivered confirmation also makes subsequent acquisition possible.
    // On failure, the durable uncertainty record continues to reject admission.
    drop(guard);
    if let Some(relay) = relay {
        let deadline = Instant::now() + Duration::from_millis(100);
        while !relay.finished.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
    }
    let _ = events.try_send(wire::Event::Terminal {
        version: wire::VERSION,
        termination: if confirmed {
            "confirmed"
        } else {
            "unconfirmed"
        },
        cause,
        forced: true,
    });
    drop(events);
    // Output failure cannot retain physical execution or block the owner forever.
    let _ = write_complete.recv_timeout(Duration::from_millis(100));
    Ok(())
}

fn exited(pid: Pid) -> io::Result<bool> {
    waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .or_else(|error| {
        if error == Errno::INTR {
            Ok(None)
        } else {
            Err(error)
        }
    })
    .map(|status| status.is_some())
    .map_err(io::Error::other)
}

fn settle(child: &mut Child, pid: Pid, budget: Duration) -> Result<(), &'static str> {
    // Never signal after reaping the group leader: a later PGID may be unrelated.
    match kill_process_group(pid, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => {}
        // macOS reports EPERM for a group containing only an exited, unreaped
        // leader. It is safe to reap only after independently observing exit.
        Err(Errno::PERM) if matches!(exited(pid), Ok(true)) => {}
        Err(_) => return Err("group_signal_failed"),
    }
    let deadline = Instant::now() + budget;
    let mut reaped = false;
    loop {
        if !reaped {
            match child.try_wait() {
                Ok(Some(_)) => reaped = true,
                Ok(None) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err("child_reap_failed"),
            }
        }
        #[cfg(target_os = "linux")]
        if reaped {
            while let Ok(Some(_)) =
                rustix::process::waitpgid(pid, rustix::process::WaitOptions::NOHANG)
            {}
        }
        if reaped && matches!(test_kill_process_group(pid), Err(Errno::SRCH)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("group_confirmation_timeout");
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn read_start() -> io::Result<wire::Start> {
    let expected = std::env::args()
        .nth(1)
        .ok_or_else(|| io::Error::other("missing exact distribution identity"))?;
    let start: wire::Start =
        wire::read(&mut io::stdin())?.ok_or_else(|| io::Error::other("missing owner start"))?;
    if start.version != wire::VERSION
        || start.distribution != expected
        || expected.is_empty()
        || expected.len() > 256
        || start.request_id != 1
        || start.stop_ms == 0
        || start.stop_ms > 60_000
        || start.confirmation_ms == 0
        || start.confirmation_ms > 60_000
        || !start.executable.is_absolute()
        || !start.root.is_absolute()
        || start.arguments.len() > 256
    {
        return Err(io::Error::other(
            "incompatible or invalid native owner start",
        ));
    }
    Ok(start)
}

fn control_io(
    stopping: Arc<AtomicBool>,
    lost: Arc<AtomicBool>,
    invalid: Arc<AtomicBool>,
    child: &mut Child,
) -> (
    mpsc::SyncSender<wire::Event>,
    mpsc::Receiver<()>,
    Option<relay::Relay>,
) {
    let (events, receive) = mpsc::sync_channel::<wire::Event>(16);
    let relay = child
        .stdin
        .take()
        .zip(child.stdout.take())
        .map(|(input, output)| relay::Relay::start(input, output, events.clone(), invalid.clone()));
    let input_relay = relay.clone();
    let input_stopping = stopping;
    let input_lost = lost.clone();
    let input_invalid = invalid;
    thread::spawn(move || {
        let mut previous = 1;
        loop {
            match wire::read::<wire::Stop>(&mut io::stdin()) {
                Ok(Some(stop)) if stop.version == wire::VERSION && stop.request_id > previous => {
                    previous = stop.request_id;
                    match stop.op {
                        wire::StopOperation::Stop if stop.message.is_none() => {
                            input_stopping.store(true, Ordering::Release);
                        }
                        wire::StopOperation::Application => {
                            if let Some((relay, message)) = input_relay.as_ref().zip(stop.message) {
                                if message.get("op").and_then(serde_json::Value::as_str)
                                    == Some("stop")
                                {
                                    relay.stop(Some(message));
                                    input_stopping.store(true, Ordering::Release);
                                } else if !relay.send(message) {
                                    input_invalid.store(true, Ordering::Release);
                                    break;
                                }
                            } else {
                                input_invalid.store(true, Ordering::Release);
                                break;
                            }
                        }
                        wire::StopOperation::Stop => {
                            input_invalid.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
                Ok(None) => {
                    input_lost.store(true, Ordering::Release);
                    break;
                }
                _ => {
                    input_invalid.store(true, Ordering::Release);
                    break;
                }
            }
        }
    });
    // A blocked or dead launcher must never block supervision or group cleanup.
    let (written, write_complete) = mpsc::sync_channel(1);
    let output_lost = lost;
    thread::spawn(move || {
        for event in receive {
            if wire::write(&mut io::stdout(), &event).is_err() {
                output_lost.store(true, Ordering::Release);
                break;
            }
        }
        let _ = written.try_send(());
    });
    (events, write_complete, relay)
}
