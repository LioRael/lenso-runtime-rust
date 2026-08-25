use std::{error::Error, fmt};

use lenso_kernel::{InvocationContext, InvocationContextError};
use serde::{Serialize, de::DeserializeOwned};

/// One application-owned typed projection over an ordinary Invocation extension.
///
/// This adds authoring-time type safety only. It does not seal or authenticate
/// the extension; use Kernel sealed extensions for authority-bearing values.
pub trait TypedExtension: Serialize + DeserializeOwned {
    /// Stable application-owned extension key.
    const KEY: &'static str;
}

/// Failure to encode, decode, or attach a typed Invocation extension.
#[derive(Debug)]
pub enum TypedExtensionError {
    /// The typed value did not encode as JSON.
    Encode(serde_json::Error),
    /// Stored extension bytes did not decode as the declared type.
    Decode(serde_json::Error),
    /// The Kernel rejected the extension key or duplicate attachment.
    Context(InvocationContextError),
}

impl fmt::Display for TypedExtensionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "typed extension encoding failed: {error}"),
            Self::Decode(error) => write!(formatter, "typed extension decoding failed: {error}"),
            Self::Context(error) => write!(formatter, "typed extension attachment failed: {error}"),
        }
    }
}

impl Error for TypedExtensionError {}

/// Typed ordinary-extension helpers for [`InvocationContext`].
pub trait CtxExt: Sized {
    /// Decodes one typed extension when the stable key is present.
    fn typed_extension<T: TypedExtension>(&self) -> Result<Option<T>, TypedExtensionError>;

    /// Encodes and attaches one typed extension without replacing an existing key.
    fn with_typed_extension<T: TypedExtension>(
        self,
        value: &T,
    ) -> Result<Self, TypedExtensionError>;
}

impl CtxExt for InvocationContext {
    fn typed_extension<T: TypedExtension>(&self) -> Result<Option<T>, TypedExtensionError> {
        self.extension(T::KEY)
            .map(|bytes| serde_json::from_slice(bytes).map_err(TypedExtensionError::Decode))
            .transpose()
    }

    fn with_typed_extension<T: TypedExtension>(
        self,
        value: &T,
    ) -> Result<Self, TypedExtensionError> {
        let bytes = serde_json::to_vec(value).map_err(TypedExtensionError::Encode)?;
        self.with_extension(T::KEY, bytes)
            .map_err(TypedExtensionError::Context)
    }
}
