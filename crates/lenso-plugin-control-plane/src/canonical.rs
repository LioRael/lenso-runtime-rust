use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// A strict authority-document or lifecycle invariant failed closed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlPlaneError {
    /// Authority JSON was not strict or did not match its schema.
    InvalidDocument { kind: &'static str, detail: String },
    /// A canonical digest was malformed or did not match the bytes.
    DigestMismatch { subject: String },
    /// Authority documents did not close over the same identity.
    AuthorityMismatch { detail: String },
    /// Store admission rejected a Bundle or Artifact.
    AdmissionRejected { detail: String },
    /// No deterministic implementation variant could be selected.
    ResolutionFailed { detail: String },
    /// A Generation transition violated fencing or lifecycle rules.
    TransitionRejected { detail: String },
    /// Host staging or shutdown failed.
    HostFailure { detail: String },
    /// A local Store operation failed.
    StoreFailure { detail: String },
}

impl fmt::Display for ControlPlaneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDocument { kind, detail } => {
                write!(formatter, "invalid {kind} document: {detail}")
            }
            Self::DigestMismatch { subject } => write!(formatter, "digest mismatch for {subject}"),
            Self::AuthorityMismatch { detail }
            | Self::AdmissionRejected { detail }
            | Self::ResolutionFailed { detail }
            | Self::TransitionRejected { detail }
            | Self::HostFailure { detail }
            | Self::StoreFailure { detail } => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for ControlPlaneError {}

/// One strict JSON authority document and its canonical content identity.
#[derive(Clone, Debug)]
pub struct CanonicalDocument<T> {
    value: T,
    bytes: Vec<u8>,
    digest: String,
}

impl<T> CanonicalDocument<T>
where
    T: DeserializeOwned + Serialize,
{
    /// Parses strict JSON, rejects duplicate/unknown/schema-invalid input, and canonicalizes it.
    pub fn parse(kind: &'static str, input: &[u8]) -> Result<Self, ControlPlaneError> {
        let value = strict_json(kind, input)?;
        Self::from_value(kind, value)
    }

    /// Constructs a canonical document from an already typed value.
    pub fn from_value(kind: &'static str, value: T) -> Result<Self, ControlPlaneError> {
        let json =
            serde_json::to_value(&value).map_err(|error| ControlPlaneError::InvalidDocument {
                kind,
                detail: error.to_string(),
            })?;
        validate_authority_value(kind, &json)?;
        let bytes =
            serde_json::to_vec(&json).map_err(|error| ControlPlaneError::InvalidDocument {
                kind,
                detail: error.to_string(),
            })?;
        let digest = sha256_digest(&bytes);
        Ok(Self {
            value,
            bytes,
            digest,
        })
    }

    /// Returns the typed authority value.
    pub const fn value(&self) -> &T {
        &self.value
    }

    /// Returns recursively key-sorted compact JSON bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the canonical SHA-256 content identity.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Consumes the document and returns its typed value.
    pub fn into_value(self) -> T {
        self.value
    }
}

/// Strictly parses one authority document while detecting duplicate keys recursively.
pub fn strict_json<T: DeserializeOwned>(
    kind: &'static str,
    input: &[u8],
) -> Result<T, ControlPlaneError> {
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let strict = StrictValue::deserialize(&mut deserializer).map_err(|error| {
        ControlPlaneError::InvalidDocument {
            kind,
            detail: error.to_string(),
        }
    })?;
    deserializer
        .end()
        .map_err(|error| ControlPlaneError::InvalidDocument {
            kind,
            detail: error.to_string(),
        })?;
    let value = strict.0;
    validate_authority_value(kind, &value)?;
    serde_json::from_value(value).map_err(|error| ControlPlaneError::InvalidDocument {
        kind,
        detail: error.to_string(),
    })
}

/// Computes the canonical digest syntax used by all control-plane documents.
pub fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

#[derive(Clone, Debug)]
struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> serde::de::Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("strict JSON authority value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(value.into())))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        u64::try_from(value)
            .map_err(|_| E::custom("negative integers are forbidden in authority documents"))
            .and_then(|value| self.visit_u64(value))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom(
            "floating-point values are forbidden in authority documents",
        ))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!("duplicate field `{key}`")));
            }
            let value = map.next_value::<StrictValue>()?;
            values.insert(key, value.0);
        }
        Ok(StrictValue(Value::Object(values)))
    }
}

fn validate_authority_value(kind: &'static str, value: &Value) -> Result<(), ControlPlaneError> {
    match value {
        Value::Number(number) if !number.is_u64() => Err(ControlPlaneError::InvalidDocument {
            kind,
            detail: "authority numbers must be non-negative integers".to_owned(),
        }),
        Value::Array(values) => {
            for value in values {
                validate_authority_value(kind, value)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_authority_value(kind, value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Example {
        schema_version: u32,
    }

    #[test]
    fn strict_json_rejects_nested_duplicates_unknowns_and_floats() {
        assert!(strict_json::<Value>("test", br#"{"a":{"x":1,"x":2}}"#).is_err());
        assert!(strict_json::<Example>("test", br#"{"schema_version":1,"x":2}"#).is_err());
        assert!(strict_json::<Value>("test", br#"{"a":1.5}"#).is_err());
        let parsed: Example = strict_json("test", br#"{"schema_version":1}"#).unwrap();
        assert_eq!(parsed.schema_version, 1);
    }
}
