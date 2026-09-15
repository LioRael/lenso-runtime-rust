//! Transport-only decoding shared by buffered and session entry points.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http::{HeaderName, HeaderValue, Request};
use serde::{Deserialize, Deserializer};

pub(super) const BODY_LIMIT: usize = 65_536;
pub(super) const HEAD_LIMIT: usize = 16_384;
const ENCODED_BODY_LIMIT: usize = 4 * BODY_LIMIT.div_ceil(3);

// Missing fields are distinct from explicit null, including in a dual-field
// envelope. Serde rejects duplicate fields; the String type also excludes
// externally tagged enum objects such as {"base64-v1":null}.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpInput {
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    #[serde(default, deserialize_with = "present")]
    body: Option<Vec<u8>>,
    #[serde(default, deserialize_with = "present")]
    body_encoding: Option<String>,
    #[serde(default, deserialize_with = "present")]
    body_base64: Option<String>,
}

pub(super) fn decode_request(input: &str) -> Result<Request<Bytes>, String> {
    // Bound serde's allocations before parsing either representation, allowing
    // the legacy numeric expansion and escaped request head as before.
    if input.len() > BODY_LIMIT * 4 + HEAD_LIMIT * 6 {
        return Err("serialized request exceeds bound".into());
    }
    let input: HttpInput = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let body = match (input.body, input.body_encoding, input.body_base64) {
        (Some(body), None, None) => body,
        (None, Some(encoding), Some(encoded)) if encoding == "base64-v1" => {
            // Reject excessive encoded and decoded lengths before allocating a
            // decoded buffer. STANDARD requires padding and zero trailing bits.
            if encoded.len() > ENCODED_BODY_LIMIT {
                return Err("encoded request body exceeds bound".into());
            }
            if !encoded.len().is_multiple_of(4) {
                return Err("invalid request body base64 length".into());
            }
            let padding = if encoded.ends_with("==") {
                2
            } else if encoded.ends_with('=') {
                1
            } else {
                0
            };
            let decoded_len = (encoded.len() / 4) * 3 - padding;
            if decoded_len > BODY_LIMIT {
                return Err("request body exceeds bound".into());
            }
            STANDARD.decode(encoded).map_err(|e| e.to_string())?
        }
        _ => return Err("invalid request body encoding fields".into()),
    };
    if body.len() > BODY_LIMIT {
        return Err("request body exceeds bound".into());
    }
    let mut request = Request::builder()
        .method(input.method.as_str())
        .uri(input.uri.as_str())
        .body(Bytes::from(body))
        .map_err(|e| e.to_string())?;
    for (name, value) in input.headers {
        request.headers_mut().append(
            HeaderName::from_bytes(name.as_bytes()).map_err(|e| e.to_string())?,
            HeaderValue::from_str(&value).map_err(|e| e.to_string())?,
        );
    }
    Ok(request)
}
