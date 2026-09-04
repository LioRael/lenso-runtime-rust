use std::collections::BTreeSet;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const LIMIT: usize = 256 * 1024;
pub const VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub version: u32,
    pub distribution: String,
    pub request_id: u32,
    pub root: std::path::PathBuf,
    pub registry: std::path::PathBuf,
    pub executable: std::path::PathBuf,
    pub arguments: Vec<String>,
    pub stop_ms: u32,
    pub confirmation_ms: u32,
    #[serde(default)]
    pub application: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stop {
    pub version: u32,
    pub request_id: u32,
    pub op: StopOperation,
    #[serde(default)]
    pub message: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopOperation {
    Stop,
    Application,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Application {
        message: serde_json::Value,
    },
    Owned {
        version: u32,
        distribution: String,
        request_id: u32,
        pid: u32,
    },
    Terminal {
        version: u32,
        termination: &'static str,
        cause: &'static str,
        forced: bool,
    },
}

pub fn read<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<Option<T>> {
    let mut header = [0_u8; 4];
    match reader.read(&mut header[..1])? {
        0 => return Ok(None),
        1 => {}
        _ => unreachable!(),
    }
    reader.read_exact(&mut header[1..])?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > LIMIT {
        return Err(io::Error::other("invalid control frame length"));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    reject_duplicate_keys(&bytes)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(io::Error::other)
}

fn reject_duplicate_keys(bytes: &[u8]) -> io::Result<()> {
    let mut objects = Vec::<BTreeSet<String>>::new();
    let mut offset = 0;
    while offset < bytes.len() {
        match bytes[offset] {
            b'{' => objects.push(BTreeSet::new()),
            b'}' => {
                objects.pop();
            }
            b'"' => {
                let begin = offset;
                offset += 1;
                while offset < bytes.len() && bytes[offset] != b'"' {
                    if bytes[offset] == b'\\' {
                        offset += 1;
                    }
                    offset += 1;
                }
                if offset >= bytes.len() {
                    break;
                }
                let mut next = offset + 1;
                while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                    next += 1;
                }
                if bytes.get(next) == Some(&b':') {
                    let key: String =
                        serde_json::from_slice(&bytes[begin..=offset]).map_err(io::Error::other)?;
                    let keys = objects
                        .last_mut()
                        .ok_or_else(|| io::Error::other("invalid control JSON key"))?;
                    if !keys.insert(key) {
                        return Err(io::Error::other("duplicate control JSON key"));
                    }
                }
            }
            _ => {}
        }
        offset += 1;
    }
    Ok(())
}

pub fn write(writer: &mut impl Write, event: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(event)?;
    if bytes.len() > LIMIT {
        return Err(io::Error::other("control frame too large"));
    }
    writer.write_all(
        &u32::try_from(bytes.len())
            .map_err(io::Error::other)?
            .to_be_bytes(),
    )?;
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_lengths_truncation_unknown_operations_and_duplicate_fields() {
        for length in [0_u32, u32::MAX] {
            assert!(read::<Stop>(&mut length.to_be_bytes().as_slice()).is_err());
        }
        for payload in [
            b"{".as_slice(),
            &[0xff],
            br#"{"version":1,"version":2,"request_id":2,"op":"stop"}"#,
            br#"{"version":1,"request_id":2,"op":"invoke"}"#,
            br#"{"version":1,"request_id":2,"op":"application","message":{"id":1,"id":2}}"#,
        ] {
            let mut bytes = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
            bytes.extend(payload);
            assert!(read::<Stop>(&mut bytes.as_slice()).is_err());
        }
        assert!(read::<Stop>(&mut [0, 0].as_slice()).is_err());
        assert!(read::<Stop>(&mut [0, 0, 0, 2, b'{'].as_slice()).is_err());
    }
}
