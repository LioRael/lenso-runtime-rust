//! Guest-side framed stdio protocol for trusted Lenso Process Plugins.
//!
//! Plugin authors use a higher-level product SDK. This crate owns process
//! framing so business code never parses Host messages.

use std::io::{self, BufRead as _, BufReader, BufWriter};

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod v2;

pub use v2::{
    GuestFrameV2, HostFrameV2, PROTOCOL_VERSION_V2, ProcessInvocationContext, ProcessPluginV2,
    ProcessStopOutcome, serve_v2, serve_v2_with_limit, serve_v2_with_profile,
    serve_v2_with_profile_and_limit,
};

/// Stable process wire identity negotiated before readiness.
pub const PROTOCOL_VERSION: &str = "lenso.process-stdio@1";

/// Default maximum encoded frame accepted by the guest SDK.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;

/// One typed-at-the-wire Process Plugin implementation.
pub trait ProcessPlugin {
    /// Canonical `lenso.json-request@1` descriptor generated from source.
    fn descriptor(&self) -> Value;

    /// Handles one request after Host descriptor validation.
    fn invoke(&self, capability: &str, operation: &str, request: Value) -> ProcessOutcome;
}

/// One terminal request outcome returned to the Host.
#[derive(Clone, Debug)]
pub enum ProcessOutcome {
    Success(Value),
    DomainError(Value),
    Failure(String),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum HostFrame {
    Invoke {
        id: u64,
        capability: String,
        operation: String,
        request: Value,
    },
    Cancel {
        id: u64,
    },
    Shutdown,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum GuestFrame<'a> {
    Ready {
        protocol: &'static str,
        descriptor: Value,
    },
    Result {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        ok: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        failure: Option<&'a str>,
    },
}

/// Serves one Process Plugin on reserved stdin/stdout until Host shutdown.
pub fn serve(plugin: &impl ProcessPlugin) -> io::Result<()> {
    serve_with_limit(plugin, DEFAULT_MAX_FRAME_BYTES)
}

/// Serves one Process Plugin with an explicit frame limit.
pub fn serve_with_limit(plugin: &impl ProcessPlugin, max_frame_bytes: usize) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    write_frame(
        &mut writer,
        &GuestFrame::Ready {
            protocol: PROTOCOL_VERSION,
            descriptor: plugin.descriptor(),
        },
    )?;

    loop {
        let Some(bytes) = read_frame(&mut reader, max_frame_bytes)? else {
            return Ok(());
        };
        let frame = serde_json::from_slice::<HostFrame>(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        match frame {
            HostFrame::Invoke {
                id,
                capability,
                operation,
                request,
            } => {
                let (ok, error, failure) = match plugin.invoke(&capability, &operation, request) {
                    ProcessOutcome::Success(value) => (Some(value), None, None),
                    ProcessOutcome::DomainError(value) => (None, Some(value), None),
                    ProcessOutcome::Failure(detail) => (None, None, Some(detail)),
                };
                write_frame(
                    &mut writer,
                    &GuestFrame::Result {
                        id,
                        ok,
                        error,
                        failure: failure.as_deref(),
                    },
                )?;
            }
            HostFrame::Cancel { id } => {
                let _ = id;
            }
            HostFrame::Shutdown => return Ok(()),
        }
    }
}

fn write_frame(writer: &mut impl io::Write, frame: &GuestFrame<'_>) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, frame)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn read_frame(reader: &mut impl io::BufRead, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    let read = io::Read::take(
        &mut *reader,
        u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1),
    )
    .read_until(b'\n', &mut bytes)?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.len() > limit || !bytes.ends_with(b"\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Lenso process frame exceeds the configured limit",
        ));
    }
    bytes.pop();
    Ok(Some(bytes))
}
