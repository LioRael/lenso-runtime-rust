//! Derivation macros for statically linked native Lenso Modules.

use std::{env, fs, path::PathBuf};

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use serde_json::{Map, Value, json};
use syn::{ItemFn, LitStr, Token, parse_macro_input};

struct ModuleAttributes {
    descriptor: Option<LitStr>,
}

impl syn::parse::Parse for ModuleAttributes {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Self { descriptor: None });
        }
        let name: syn::Ident = input.parse()?;
        if name != "descriptor" {
            return Err(syn::Error::new(name.span(), "expected `descriptor`"));
        }
        input.parse::<Token![=]>()?;
        let descriptor = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("unexpected Module attribute input"));
        }
        Ok(Self {
            descriptor: Some(descriptor),
        })
    }
}

/// Derives the native factory and link-time registration for one Module entrypoint.
///
/// The package identity comes from `[package.metadata.lenso].package-id` in the
/// consuming crate's `Cargo.toml`; the package version remains Cargo-owned.
#[proc_macro_attribute]
pub fn module(attributes: TokenStream, item: TokenStream) -> TokenStream {
    let attributes = parse_macro_input!(attributes as ModuleAttributes);
    let function = parse_macro_input!(item as ItemFn);
    expand_module(&attributes, &function)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_module(
    attributes: &ModuleAttributes,
    function: &ItemFn,
) -> syn::Result<proc_macro2::TokenStream> {
    let package_id = package_id()?;
    let descriptor_json = attributes
        .descriptor
        .as_ref()
        .map(|descriptor| module_descriptor(&package_id, descriptor))
        .transpose()?;
    let function_name = &function.sig.ident;
    let generated_module = format_ident!("__lenso_module_{function_name}");
    let descriptor_constant = descriptor_json.map(|descriptor| {
        let artifact =
            format!("LENSO_MODULE_DESCRIPTOR_V1\0{descriptor}\0END_LENSO_MODULE_DESCRIPTOR_V1");
        let artifact_length = artifact.len();
        let artifact = proc_macro2::Literal::byte_string(artifact.as_bytes());
        quote! {
            /// Generated package-owned Module Descriptor bytes.
            pub const MODULE_DESCRIPTOR_JSON: &str = #descriptor;
            /// Linker-retained descriptor artifact consumed without executing package code.
            #[doc(hidden)]
            #[used]
            pub static __LENSO_MODULE_DESCRIPTOR_ARTIFACT: [u8; #artifact_length] = *#artifact;
        }
    });

    Ok(quote! {
        /// Runtime package identity derived from Cargo package metadata.
        pub const PACKAGE_ID: &str = #package_id;
        /// Exact linked Cargo package version.
        pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
        /// Exact Host Build identity for this linked package.
        pub const FACTORY_IDENTITY: &str = concat!(#package_id, "@", env!("CARGO_PKG_VERSION"));
        #descriptor_constant

        #function

        #[doc(hidden)]
        mod #generated_module {
            #[derive(Clone, Copy, Debug, Default)]
            struct Factory;

            impl ::lenso_native_adapter::NativeModuleFactory for Factory {
                fn package_id(&self) -> &'static str {
                    #package_id
                }

                fn package_version(&self) -> &'static str {
                    env!("CARGO_PKG_VERSION")
                }

                fn instantiate(
                    &self,
                    context: ::lenso_native_adapter::NativeModuleFactoryContext<'_>,
                ) -> Result<
                    ::lenso_native_adapter::NativeModuleInstance,
                    ::lenso_native_adapter::RuntimeFailure,
                > {
                    super::#function_name(context)
                }
            }

            fn factory() -> std::rc::Rc<dyn ::lenso_native_adapter::NativeModuleFactory> {
                std::rc::Rc::new(Factory)
            }

            ::lenso_native_adapter::__inventory::submit! {
                ::lenso_native_adapter::LinkedNativeModuleFactory::new(factory)
            }

            // Make Cargo track the manifest that supplied the generated identity.
            const _: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        }
    })
}

fn module_descriptor(package_id: &str, descriptor: &LitStr) -> syn::Result<String> {
    let supplied: Value = serde_json::from_str(&descriptor.value()).map_err(|error| {
        syn::Error::new(
            descriptor.span(),
            format!("Module Descriptor input is not valid JSON: {error}"),
        )
    })?;
    let supplied = supplied.as_object().cloned().ok_or_else(|| {
        syn::Error::new(
            descriptor.span(),
            "Module Descriptor input must be an object",
        )
    })?;
    for owned in [
        "package_id",
        "package_revision",
        "entrypoint",
        "execution_class",
        "restart_policy",
        "criticality",
    ] {
        if supplied.contains_key(owned) {
            return Err(syn::Error::new(
                descriptor.span(),
                format!("Module Descriptor input cannot override generated field `{owned}`"),
            ));
        }
    }
    let package_version = env::var("CARGO_PKG_VERSION").map_err(|_| {
        syn::Error::new(
            descriptor.span(),
            "CARGO_PKG_VERSION is unavailable while deriving Module Descriptor",
        )
    })?;
    Ok(complete_module_descriptor(
        package_id,
        &package_version,
        supplied,
    ))
}

fn complete_module_descriptor(
    package_id: &str,
    package_version: &str,
    mut supplied: Map<String, Value>,
) -> String {
    let mut generated = Map::new();
    generated.insert("package_id".to_owned(), json!(package_id));
    generated.insert("package_revision".to_owned(), json!(package_version));
    generated.insert("entrypoint".to_owned(), json!("default"));
    for (key, value) in std::mem::take(&mut supplied) {
        generated.insert(key, value);
    }
    generated.insert("execution_class".to_owned(), json!("lenso.native-rust@1"));
    generated.insert(
        "restart_policy".to_owned(),
        json!({
            "mode": "never",
            "max_attempts": 0,
            "window": {"secs": 0, "nanos": 0},
            "backoff": {"secs": 0, "nanos": 0},
            "stability": {"secs": 0, "nanos": 0},
            "jitter": {"secs": 0, "nanos": 0}
        }),
    );
    generated.insert("criticality".to_owned(), json!("non_critical"));
    serde_json::to_string(&Value::Object(generated))
        .expect("generated Module Descriptor values must serialize")
}

fn package_id() -> syn::Result<String> {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "CARGO_MANIFEST_DIR is unavailable",
        )
    })?;
    let manifest_path = PathBuf::from(manifest_dir).join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path).map_err(|error| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("failed to read {}: {error}", manifest_path.display()),
        )
    })?;
    let manifest: toml::Value = toml::from_str(&manifest).map_err(|error| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("failed to parse {}: {error}", manifest_path.display()),
        )
    })?;
    manifest
        .get("package")
        .and_then(|package| package.get("metadata"))
        .and_then(|metadata| metadata.get("lenso"))
        .and_then(|lenso| lenso.get("package-id"))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                "missing `[package.metadata.lenso] package-id = \"...\"` in Cargo.toml",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_descriptor_owns_identity_and_execution_defaults() {
        let supplied = serde_json::from_value::<Map<String, Value>>(json!({
            "provided_capabilities": [],
            "required_capabilities": []
        }))
        .unwrap();
        let descriptor = complete_module_descriptor("example.tool", "1.2.3", supplied);
        let descriptor: Value = serde_json::from_str(&descriptor).unwrap();

        assert_eq!(descriptor["package_id"], "example.tool");
        assert_eq!(descriptor["package_revision"], "1.2.3");
        assert_eq!(descriptor["entrypoint"], "default");
        assert_eq!(descriptor["execution_class"], "lenso.native-rust@1");
        assert_eq!(descriptor["restart_policy"]["mode"], "never");
        assert_eq!(descriptor["criticality"], "non_critical");
    }
}
