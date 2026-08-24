//! Derivation macros for statically linked native Lenso Modules.

use std::{env, fs, path::PathBuf};

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

/// Derives the native factory and link-time registration for one Module entrypoint.
///
/// The package identity comes from `[package.metadata.lenso].package-id` in the
/// consuming crate's `Cargo.toml`; the package version remains Cargo-owned.
#[proc_macro_attribute]
pub fn module(attributes: TokenStream, item: TokenStream) -> TokenStream {
    if !attributes.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[lenso::module] does not accept identity arguments; declare package-id in Cargo.toml",
        )
        .into_compile_error()
        .into();
    }

    let function = parse_macro_input!(item as ItemFn);
    expand_module(&function)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_module(function: &ItemFn) -> syn::Result<proc_macro2::TokenStream> {
    let package_id = package_id()?;
    let function_name = &function.sig.ident;
    let generated_module = format_ident!("__lenso_module_{function_name}");

    Ok(quote! {
        /// Runtime package identity derived from Cargo package metadata.
        pub const PACKAGE_ID: &str = #package_id;
        /// Exact linked Cargo package version.
        pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
        /// Exact Host Build identity for this linked package.
        pub const FACTORY_IDENTITY: &str = concat!(#package_id, "@", env!("CARGO_PKG_VERSION"));

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
