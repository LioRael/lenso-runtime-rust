//! Derivation macros for statically linked native Lenso Modules.

use std::{env, fs, path::PathBuf};

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use serde_json::{Map, Value, json};
use syn::{
    Attribute, Data, DeriveInput, Fields, GenericArgument, Item, ItemFn, ItemImpl, ItemStruct,
    LitStr, Path, PathArguments, Token, Type, parse_macro_input,
};

struct ModuleAttributes {
    descriptor: Option<LitStr>,
    configuration_schema: Option<LitStr>,
    validate: Option<Path>,
    prepare: Option<Path>,
    activate: Option<Path>,
    deactivate: Option<Path>,
}

impl syn::parse::Parse for ModuleAttributes {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Self {
                descriptor: None,
                configuration_schema: None,
                validate: None,
                prepare: None,
                activate: None,
                deactivate: None,
            });
        }
        let mut descriptor = None;
        let mut configuration_schema = None;
        let mut validate = None;
        let mut prepare = None;
        let mut activate = None;
        let mut deactivate = None;
        while !input.is_empty() {
            let name: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match name.to_string().as_str() {
                "descriptor" if descriptor.is_none() => descriptor = Some(input.parse()?),
                "configuration_schema" if configuration_schema.is_none() => {
                    configuration_schema = Some(input.parse()?);
                }
                "validate" if validate.is_none() => validate = Some(input.parse()?),
                "prepare" if prepare.is_none() => prepare = Some(input.parse()?),
                "activate" if activate.is_none() => activate = Some(input.parse()?),
                "deactivate" if deactivate.is_none() => deactivate = Some(input.parse()?),
                "descriptor"
                | "configuration_schema"
                | "validate"
                | "prepare"
                | "activate"
                | "deactivate" => {
                    return Err(syn::Error::new(name.span(), "duplicate Module attribute"));
                }
                _ => {
                    return Err(syn::Error::new(
                        name.span(),
                        "expected `descriptor`, `configuration_schema`, `validate`, `prepare`, `activate`, or `deactivate`",
                    ));
                }
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(Self {
            descriptor,
            configuration_schema,
            validate,
            prepare,
            activate,
            deactivate,
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
    let item = parse_macro_input!(item as Item);
    match item {
        Item::Fn(function) => expand_module_function(&attributes, &function),
        Item::Struct(module) => expand_module_struct(&attributes, module),
        other => Err(syn::Error::new_spanned(
            other,
            "a native Module must be declared by a factory function or a named-field struct",
        )),
    }
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}

/// Derives the locked JSON Schema fragment consumed by a struct-level Module.
#[proc_macro_derive(ModuleConfig, attributes(serde))]
pub fn module_config(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    expand_module_config(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_module_config(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "Module configuration must be a named-field struct",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &data.fields,
            "Module configuration must use named fields",
        ));
    };
    let mut properties = Map::new();
    let mut required = Vec::new();
    for field in &fields.named {
        let ident = field.ident.as_ref().expect("named fields have identifiers");
        let name = serde_field_name(&field.attrs, ident)?;
        let (schema, optional) = configuration_type_schema(&field.ty)?;
        properties.insert(name.clone(), schema);
        if !optional {
            required.push(Value::String(name));
        }
    }
    let schema = canonical_json(&json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties,
    }));
    let macro_name = format_ident!("__lenso_config_schema_{}", snake(&input.ident.to_string()));
    Ok(quote! {
        #[doc(hidden)]
        #[macro_export]
        macro_rules! #macro_name {
            () => { #schema };
        }
    })
}

fn serde_field_name(attributes: &[Attribute], ident: &syn::Ident) -> syn::Result<String> {
    let mut name = ident.to_string();
    for attribute in attributes {
        if !attribute.path().is_ident("serde") {
            continue;
        }
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                name = meta.value()?.parse::<LitStr>()?.value();
            }
            Ok(())
        })?;
    }
    Ok(name)
}

fn configuration_type_schema(ty: &Type) -> syn::Result<(Value, bool)> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "Module configuration fields must use portable named types",
        ));
    };
    let segment = path.path.segments.last().expect("type paths are non-empty");
    let name = segment.ident.to_string();
    if name == "Option" {
        return Ok((configuration_inner_schema(segment, ty)?, true));
    }
    if name == "Vec" {
        return Ok((
            json!({"type": "array", "items": configuration_inner_schema(segment, ty)?}),
            false,
        ));
    }
    let schema = match name.as_str() {
        "String" => json!({"type": "string"}),
        "bool" => json!({"type": "boolean"}),
        "f32" | "f64" => json!({"type": "number"}),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => json!({"type": "integer"}),
        _ => {
            return Err(syn::Error::new_spanned(
                ty,
                "unsupported Module configuration field type; use String, bool, a number, Option<T>, or Vec<T>",
            ));
        }
    };
    Ok((schema, false))
}

fn configuration_inner_schema(segment: &syn::PathSegment, ty: &Type) -> syn::Result<Value> {
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "configuration container requires one type",
        ));
    };
    let [GenericArgument::Type(inner)] = arguments.args.iter().collect::<Vec<_>>().as_slice()
    else {
        return Err(syn::Error::new_spanned(
            ty,
            "configuration container requires one type",
        ));
    };
    configuration_type_schema(inner).map(|(schema, _)| schema)
}

fn expand_module_function(
    attributes: &ModuleAttributes,
    function: &ItemFn,
) -> syn::Result<proc_macro2::TokenStream> {
    if attributes.descriptor.is_none() && attributes.configuration_schema.is_some() {
        return Err(syn::Error::new_spanned(
            function,
            "`configuration_schema` requires `descriptor` on a factory function",
        ));
    }
    if attributes.validate.is_some()
        || attributes.prepare.is_some()
        || attributes.activate.is_some()
        || attributes.deactivate.is_some()
    {
        return Err(syn::Error::new_spanned(
            function,
            "lifecycle hooks are available only on struct-level Modules",
        ));
    }
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

/// Derives one provided Capability endpoint, native factory, and static registration.
///
/// Apply this to the generated Capability Provider trait implementation for a
/// struct already annotated with [`module`].
#[proc_macro_attribute]
pub fn provides(attributes: TokenStream, item: TokenStream) -> TokenStream {
    let capability = parse_macro_input!(attributes as Path);
    let implementation = parse_macro_input!(item as ItemImpl);
    expand_provides(&capability, &implementation)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_provides(
    capability: &Path,
    implementation: &ItemImpl,
) -> syn::Result<proc_macro2::TokenStream> {
    if implementation.trait_.is_none() {
        return Err(syn::Error::new_spanned(
            implementation,
            "`provides` must annotate a generated Capability Provider trait implementation",
        ));
    }
    let Type::Path(module_type) = implementation.self_ty.as_ref() else {
        return Err(syn::Error::new_spanned(
            &implementation.self_ty,
            "the Module provider type must be a path",
        ));
    };
    let module_name = module_type.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(&module_type.path, "the Module provider type is empty")
    })?;
    let module_ident = &module_name.ident;
    let mut capability_namespace = capability.clone();
    let capability_ident = capability_namespace
        .segments
        .pop()
        .ok_or_else(|| syn::Error::new_spanned(capability, "Capability path is empty"))?
        .into_value()
        .ident;
    capability_namespace.segments.pop_punct();
    if capability_namespace.segments.is_empty() {
        return Err(syn::Error::new_spanned(
            capability,
            "Capability must be namespace-qualified, for example `agent::Agent`",
        ));
    }
    let module_descriptor = format_ident!(
        "__lenso_module_descriptor_{}",
        snake(&module_ident.to_string())
    );
    let capability_snake = snake(&capability_ident.to_string());
    let provided_descriptor = format_ident!("__lenso_provided_{capability_snake}");
    let provide_endpoint = format_ident!("__lenso_native_provide_{capability_snake}");
    let generated_module = format_ident!("__lenso_provider_{}", snake(&module_ident.to_string()));
    let lifecycle = format_ident!("__LensoLifecycle{module_ident}");
    let artifact = format_ident!("__LENSO_MODULE_DESCRIPTOR_ARTIFACT_{module_ident}");

    Ok(quote! {
        #implementation

        /// Generated package-owned Module Descriptor bytes.
        pub const MODULE_DESCRIPTOR_JSON: &str = #module_descriptor!(
            #capability_namespace::#provided_descriptor!()
        );
        #[doc(hidden)]
        const __LENSO_MODULE_DESCRIPTOR_ARTIFACT_TEXT: &str = concat!(
            "LENSO_MODULE_DESCRIPTOR_V1\0",
            #module_descriptor!(#capability_namespace::#provided_descriptor!()),
            "\0END_LENSO_MODULE_DESCRIPTOR_V1",
        );
        /// Linker-retained descriptor artifact consumed without executing package code.
        #[doc(hidden)]
        #[used]
        pub static #artifact: &[u8] = __LENSO_MODULE_DESCRIPTOR_ARTIFACT_TEXT.as_bytes();

        #[doc(hidden)]
        mod #generated_module {
            #[derive(Clone, Copy, Debug, Default)]
            struct Factory;

            impl ::lenso_native_adapter::NativeModuleFactory for Factory {
                fn package_id(&self) -> &'static str { super::PACKAGE_ID }
                fn package_version(&self) -> &'static str { super::PACKAGE_VERSION }

                fn instantiate(
                    &self,
                    context: ::lenso_native_adapter::NativeModuleFactoryContext<'_>,
                ) -> Result<
                    ::lenso_native_adapter::NativeModuleInstance,
                    ::lenso_native_adapter::RuntimeFailure,
                > {
                    let module = super::#module_ident::__lenso_construct(context)?;
                    let lifecycle = super::#lifecycle { module: module.clone() };
                    Ok(super::#capability_namespace::#provide_endpoint!(module, lifecycle))
                }
            }

            fn factory() -> ::std::rc::Rc<dyn ::lenso_native_adapter::NativeModuleFactory> {
                ::std::rc::Rc::new(Factory)
            }

            ::lenso_native_adapter::__inventory::submit! {
                ::lenso_native_adapter::LinkedNativeModuleFactory::new(factory)
            }
        }
    })
}

fn expand_module_struct(
    attributes: &ModuleAttributes,
    mut module: ItemStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    if attributes.descriptor.is_some() {
        return Err(syn::Error::new_spanned(
            &module.ident,
            "struct-level Modules derive their Descriptor; remove `descriptor`",
        ));
    }
    let package_id = package_id()?;
    let package_version = env::var("CARGO_PKG_VERSION").map_err(|_| {
        syn::Error::new_spanned(
            &module.ident,
            "CARGO_PKG_VERSION is unavailable while deriving Module Descriptor",
        )
    })?;
    let StructFields {
        config_type,
        ports,
        initializers,
    } = analyze_struct_fields(&mut module)?;
    let schema =
        configuration_schema_tokens(attributes.configuration_schema.as_ref(), &config_type)?;
    let name = &module.ident;
    let lifecycle_name = format_ident!("__LensoLifecycle{name}");
    let descriptor_macro = format_ident!("__lenso_module_descriptor_{}", snake(&name.to_string()));
    let requirement_macros = ports
        .iter()
        .map(|(_, client)| requirement_macro(client))
        .collect::<syn::Result<Vec<_>>>()?;
    let connect_ports = ports.iter().map(|(field, _)| {
        quote! { self.module.#field.connect(context.dependencies())?; }
    });
    let requirement_parts = intersperse_commas(requirement_macros);
    let (prefix, after_schema, suffix, defaults) =
        descriptor_affixes(&package_id, &package_version);
    let validate = attributes
        .validate
        .as_ref()
        .map(|path| quote!(#path(&configuration)?;));
    let prepare = hook(attributes.prepare.as_ref());
    let activate = hook(attributes.activate.as_ref());
    let deactivate = hook(attributes.deactivate.as_ref());
    let schema_tracking = attributes.configuration_schema.as_ref().map(|path| {
        quote!(
            const _: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", #path));
        )
    });

    Ok(quote! {
        /// Runtime package identity derived from Cargo package metadata.
        pub const PACKAGE_ID: &str = #package_id;
        /// Exact linked Cargo package version.
        pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
        /// Exact Host Build identity for this linked package.
        pub const FACTORY_IDENTITY: &str = concat!(#package_id, "@", env!("CARGO_PKG_VERSION"));

        #module

        #[doc(hidden)]
        macro_rules! #descriptor_macro {
            ($provided:expr) => {
                concat!(#prefix, #schema, #after_schema, $provided, #suffix #(, #requirement_parts)*, #defaults)
            };
        }

        impl #name {
            #[doc(hidden)]
            fn __lenso_construct(
                context: ::lenso_native_adapter::NativeModuleFactoryContext<'_>,
            ) -> Result<Self, ::lenso_native_adapter::RuntimeFailure> {
                if context.entrypoint() != "default" {
                    return Err(::lenso_native_adapter::RuntimeFailure::InvalidResolvedPlan {
                        detail: format!("unsupported {} entrypoint {}", #package_id, context.entrypoint()),
                    });
                }
                let configuration = ::serde_json::from_str::<#config_type>(context.configuration())
                    .map_err(|error| ::lenso_native_adapter::RuntimeFailure::InvalidResolvedPlan {
                        detail: format!("invalid {} configuration: {error}", #package_id),
                    })?;
                #validate
                Ok(Self { #(#initializers),* })
            }
        }

        #[doc(hidden)]
        #[derive(Clone, Debug)]
        struct #lifecycle_name {
            module: #name,
        }

        impl ::lenso_kernel::ModuleLifecycle for #lifecycle_name {
            fn prepare(&self, context: ::lenso_kernel::PrepareContext) -> ::lenso_kernel::ModuleFuture {
                #prepare
            }

            fn activate(&self, context: ::lenso_kernel::ActivateContext) -> ::lenso_kernel::ModuleFuture {
                let connected = (|| -> Result<(), ::lenso_native_adapter::RuntimeFailure> {
                    #(#connect_ports)*
                    Ok(())
                })();
                if let Err(error) = connected {
                    return Box::pin(::futures::future::ready(Err(error)));
                }
                #activate
            }

            fn deactivate(&self, context: ::lenso_kernel::DeactivateContext) -> ::lenso_kernel::ModuleFuture {
                #deactivate
            }
        }

        const _: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        #schema_tracking
    })
}

fn descriptor_affixes(
    package_id: &str,
    package_version: &str,
) -> (String, &'static str, &'static str, &'static str) {
    let prefix = format!(
        "{{\"package_id\":{},\"package_revision\":{},\"entrypoint\":\"default\",\"configuration_schema\":",
        serde_json::to_string(package_id).expect("package ID serializes"),
        serde_json::to_string(package_version).expect("package version serializes"),
    );
    let after_schema = ",\"provided_capabilities\":[";
    let suffix = "],\"required_capabilities\":[";
    let defaults = "],\"execution_class\":\"lenso.native-rust@1\",\"restart_policy\":{\"mode\":\"never\",\"max_attempts\":0,\"window\":{\"secs\":0,\"nanos\":0},\"backoff\":{\"secs\":0,\"nanos\":0},\"stability\":{\"secs\":0,\"nanos\":0},\"jitter\":{\"secs\":0,\"nanos\":0}},\"criticality\":\"non_critical\"}";
    (prefix, after_schema, suffix, defaults)
}

fn configuration_schema_tokens(
    schema_path: Option<&LitStr>,
    config_type: &Type,
) -> syn::Result<proc_macro2::TokenStream> {
    if let Some(path) = schema_path {
        let schema = canonical_json(&read_configuration_schema(path)?);
        return Ok(quote!(#schema));
    }
    let Type::Path(config) = config_type else {
        return Err(syn::Error::new_spanned(
            config_type,
            "the `#[config]` field type must be a path",
        ));
    };
    let mut namespace = config.path.clone();
    let config_name = namespace
        .segments
        .pop()
        .expect("type paths are non-empty")
        .into_value()
        .ident;
    namespace.segments.pop_punct();
    let macro_name = format_ident!("__lenso_config_schema_{}", snake(&config_name.to_string()));
    if namespace.segments.is_empty() {
        Ok(quote!(#macro_name!()))
    } else {
        Ok(quote!(#namespace::#macro_name!()))
    }
}

struct StructFields {
    config_type: Type,
    ports: Vec<(syn::Ident, Path)>,
    initializers: Vec<proc_macro2::TokenStream>,
}

fn analyze_struct_fields(module: &mut ItemStruct) -> syn::Result<StructFields> {
    let Fields::Named(fields) = &mut module.fields else {
        return Err(syn::Error::new_spanned(
            &module.fields,
            "a struct-level Module requires named fields",
        ));
    };
    let mut config = None;
    let mut ports = Vec::new();
    let mut initializers = Vec::new();
    for field in &mut fields.named {
        let name = field.ident.as_ref().expect("named fields have identifiers");
        if take_marker(&mut field.attrs, "config") {
            if config.replace(field.ty.clone()).is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "a Module has exactly one `#[config]` field",
                ));
            }
            initializers.push(quote!(#name: configuration));
        } else if let Some(client) = port_client(&field.ty)? {
            ports.push((name.clone(), client));
            initializers.push(quote!(#name: ::core::default::Default::default()));
        } else {
            initializers.push(quote!(#name: ::core::default::Default::default()));
        }
    }
    let Some(config_type) = config else {
        return Err(syn::Error::new_spanned(
            &module.ident,
            "a struct-level Module requires one `#[config]` field",
        ));
    };
    Ok(StructFields {
        config_type,
        ports,
        initializers,
    })
}

fn take_marker(attributes: &mut Vec<Attribute>, name: &str) -> bool {
    let present = attributes
        .iter()
        .any(|attribute| attribute.path().is_ident(name));
    attributes.retain(|attribute| !attribute.path().is_ident(name));
    present
}

fn port_client(ty: &Type) -> syn::Result<Option<Path>> {
    let Type::Path(path) = ty else {
        return Ok(None);
    };
    let Some(segment) = path.path.segments.last() else {
        return Ok(None);
    };
    if segment.ident != "Port" {
        return Ok(None);
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "Port requires one Capability client type",
        ));
    };
    let [syn::GenericArgument::Type(Type::Path(client))] =
        arguments.args.iter().collect::<Vec<_>>().as_slice()
    else {
        return Err(syn::Error::new_spanned(
            ty,
            "Port requires one Capability client type",
        ));
    };
    Ok(Some(client.path.clone()))
}

fn requirement_macro(client: &Path) -> syn::Result<proc_macro2::TokenStream> {
    if client.segments.len() < 2 {
        return Err(syn::Error::new_spanned(
            client,
            "a Port client must be namespace-qualified, for example `model::ModelClient`",
        ));
    }
    let mut namespace = client.clone();
    let client_name = namespace
        .segments
        .pop()
        .expect("checked length")
        .into_value()
        .ident;
    namespace.segments.pop_punct();
    let macro_name = format_ident!("__lenso_required_{}", snake(&client_name.to_string()));
    Ok(quote!(#namespace::#macro_name!()))
}

fn intersperse_commas(values: Vec<proc_macro2::TokenStream>) -> Vec<proc_macro2::TokenStream> {
    values
        .into_iter()
        .enumerate()
        .flat_map(|(index, value)| {
            if index == 0 {
                vec![value]
            } else {
                vec![quote!(","), value]
            }
        })
        .collect()
}

fn hook(path: Option<&Path>) -> proc_macro2::TokenStream {
    path.map_or_else(
        || quote!(Box::pin(::futures::future::ready(Ok(())))),
        |path| quote!(#path(&self.module, context)),
    )
}

fn canonical_json(value: &Value) -> String {
    serde_json::to_string(value).expect("JSON values serialize")
}

fn snake(value: &str) -> String {
    let mut output = String::new();
    for (index, character) in value.chars().enumerate() {
        if character.is_ascii_uppercase() && index > 0 {
            output.push('_');
        }
        output.push(character.to_ascii_lowercase());
    }
    output
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
