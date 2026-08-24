//! Derivation macros for statically linked native Lenso Modules.

use std::{env, fs, path::PathBuf};

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use serde_json::{Map, Value, json};
use syn::{ItemFn, LitStr, Token, parse_macro_input};

struct ModuleAttributes {
    descriptor: Option<LitStr>,
    configuration_schema: Option<LitStr>,
}

impl syn::parse::Parse for ModuleAttributes {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Self {
                descriptor: None,
                configuration_schema: None,
            });
        }
        let mut descriptor = None;
        let mut configuration_schema = None;
        while !input.is_empty() {
            let name: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: LitStr = input.parse()?;
            match name.to_string().as_str() {
                "descriptor" if descriptor.is_none() => descriptor = Some(value),
                "configuration_schema" if configuration_schema.is_none() => {
                    configuration_schema = Some(value);
                }
                "descriptor" | "configuration_schema" => {
                    return Err(syn::Error::new(name.span(), "duplicate Module attribute"));
                }
                _ => {
                    return Err(syn::Error::new(
                        name.span(),
                        "expected `descriptor` or `configuration_schema`",
                    ));
                }
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        if descriptor.is_none() && configuration_schema.is_some() {
            return Err(input.error("`configuration_schema` requires `descriptor`"));
        }
        Ok(Self {
            descriptor,
            configuration_schema,
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
        .map(|descriptor| {
            module_descriptor(
                &package_id,
                descriptor,
                attributes.configuration_schema.as_ref(),
            )
        })
        .transpose()?;
    let function_name = &function.sig.ident;
    let generated_module = format_ident!("__lenso_module_{function_name}");
    let descriptor_constant = descriptor_json.map(|descriptor| {
        let artifact =
            format!("LENSO_MODULE_DESCRIPTOR_V1\0{descriptor}\0END_LENSO_MODULE_DESCRIPTOR_V1");
        let artifact_length = artifact.len();
        let artifact = proc_macro2::Literal::byte_string(artifact.as_bytes());
        let schema_tracking = attributes.configuration_schema.as_ref().map(|schema| {
            quote! {
                const _: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", #schema));
            }
        });
        quote! {
            /// Generated package-owned Module Descriptor bytes.
            pub const MODULE_DESCRIPTOR_JSON: &str = #descriptor;
            /// Linker-retained descriptor artifact consumed without executing package code.
            #[doc(hidden)]
            #[used]
            pub static __LENSO_MODULE_DESCRIPTOR_ARTIFACT: [u8; #artifact_length] = *#artifact;
            #schema_tracking
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

fn module_descriptor(
    package_id: &str,
    descriptor: &LitStr,
    configuration_schema: Option<&LitStr>,
) -> syn::Result<String> {
    let supplied: Value = serde_json::from_str(&descriptor.value()).map_err(|error| {
        syn::Error::new(
            descriptor.span(),
            format!("Module Descriptor input is not valid JSON: {error}"),
        )
    })?;
    let mut supplied = supplied.as_object().cloned().ok_or_else(|| {
        syn::Error::new(
            descriptor.span(),
            "Module Descriptor input must be an object",
        )
    })?;
    if supplied.contains_key("configuration_schema") {
        return Err(syn::Error::new(
            descriptor.span(),
            "Module Descriptor input cannot contain `configuration_schema`; use the package-owned schema path attribute",
        ));
    }
    if let Some(schema_path) = configuration_schema {
        supplied.insert(
            "configuration_schema".to_owned(),
            read_configuration_schema(schema_path)?,
        );
    }
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

fn read_configuration_schema(schema_path: &LitStr) -> syn::Result<Value> {
    let relative = PathBuf::from(schema_path.value());
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(syn::Error::new(
            schema_path.span(),
            "configuration Schema path must stay inside the Module package",
        ));
    }
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| {
        syn::Error::new(
            schema_path.span(),
            "CARGO_MANIFEST_DIR is unavailable while deriving configuration Schema",
        )
    })?;
    let path = PathBuf::from(manifest_dir).join(relative);
    let bytes = fs::read(&path).map_err(|error| {
        syn::Error::new(
            schema_path.span(),
            format!(
                "failed to read configuration Schema {}: {error}",
                path.display()
            ),
        )
    })?;
    let schema: Value = serde_json::from_slice(&bytes).map_err(|error| {
        syn::Error::new(
            schema_path.span(),
            format!(
                "configuration Schema {} is invalid JSON: {error}",
                path.display()
            ),
        )
    })?;
    if !schema.is_object() {
        return Err(syn::Error::new(
            schema_path.span(),
            "configuration Schema must be a JSON object",
        ));
    }
    Ok(schema)
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

    #[test]
    fn package_schema_is_embedded_as_descriptor_data() {
        let path = LitStr::new(
            "tests/fixtures/config.schema.json",
            proc_macro2::Span::call_site(),
        );
        let schema = read_configuration_schema(&path).unwrap();

        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["name"]));
    }
}
