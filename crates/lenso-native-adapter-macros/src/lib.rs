//! Derivation macros for statically linked Lenso Plugins and built-in Plugins.

use std::{collections::BTreeSet, env, fs, path::PathBuf};

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use quote::{format_ident, quote};
use serde_json::{Map, Value, json};
use syn::{
    Attribute, Data, DeriveInput, Expr, Fields, GenericArgument, Item, ItemFn, ItemImpl,
    ItemStruct, LitStr, Path, PathArguments, Token, Type, parse_macro_input,
    punctuated::Punctuated,
};

struct PluginAttributes {
    descriptor: Option<LitStr>,
    configuration_schema: Option<LitStr>,
    configuration_defaults: Option<LitStr>,
    validate: Option<Path>,
    prepare: Option<Path>,
    activate: Option<Path>,
    deactivate: Option<Path>,
    lifecycle: bool,
    consumer: bool,
}

impl syn::parse::Parse for PluginAttributes {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Self {
                descriptor: None,
                configuration_schema: None,
                configuration_defaults: None,
                validate: None,
                prepare: None,
                activate: None,
                deactivate: None,
                lifecycle: false,
                consumer: false,
            });
        }
        let mut descriptor = None;
        let mut configuration_schema = None;
        let mut configuration_defaults = None;
        let mut validate = None;
        let mut prepare = None;
        let mut activate = None;
        let mut deactivate = None;
        let mut lifecycle = false;
        let mut consumer = false;
        while !input.is_empty() {
            let name: syn::Ident = input.parse()?;
            if name == "lifecycle" {
                if lifecycle {
                    return Err(syn::Error::new(name.span(), "duplicate Plugin attribute"));
                }
                lifecycle = true;
                if input.is_empty() {
                    break;
                }
                input.parse::<Token![,]>()?;
                continue;
            }
            if name == "consumer" {
                if consumer {
                    return Err(syn::Error::new(name.span(), "duplicate Plugin attribute"));
                }
                consumer = true;
                if input.is_empty() {
                    break;
                }
                input.parse::<Token![,]>()?;
                continue;
            }
            input.parse::<Token![=]>()?;
            match name.to_string().as_str() {
                "descriptor" if descriptor.is_none() => descriptor = Some(input.parse()?),
                "configuration_schema" if configuration_schema.is_none() => {
                    configuration_schema = Some(input.parse()?);
                }
                "configuration_defaults" if configuration_defaults.is_none() => {
                    configuration_defaults = Some(input.parse()?);
                }
                "validate" if validate.is_none() => validate = Some(input.parse()?),
                "prepare" if prepare.is_none() => prepare = Some(input.parse()?),
                "activate" if activate.is_none() => activate = Some(input.parse()?),
                "deactivate" if deactivate.is_none() => deactivate = Some(input.parse()?),
                "descriptor"
                | "configuration_schema"
                | "configuration_defaults"
                | "validate"
                | "prepare"
                | "activate"
                | "deactivate" => {
                    return Err(syn::Error::new(name.span(), "duplicate Plugin attribute"));
                }
                _ => {
                    return Err(syn::Error::new(
                        name.span(),
                        "expected `descriptor`, `configuration_schema`, `configuration_defaults`, `validate`, `prepare`, `activate`, `deactivate`, `lifecycle`, or `consumer`",
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
            configuration_defaults,
            validate,
            prepare,
            activate,
            deactivate,
            lifecycle,
            consumer,
        })
    }
}

/// Derives native Plugin source and, for `consumer`, its factory registration.
///
/// ```compile_fail
/// use lenso_native_adapter_macros::plugin;
///
/// #[plugin]
/// enum InvalidPlugin {}
/// ```
#[proc_macro_attribute]
pub fn plugin(attributes: TokenStream, item: TokenStream) -> TokenStream {
    expand_authoring_item(attributes, item)
}

/// Binds an optional complete-object constructor and stop hook to a Plugin type.
#[proc_macro_attribute]
pub fn plugin_impl(attributes: TokenStream, item: TokenStream) -> TokenStream {
    if !attributes.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[plugin_impl] does not accept arguments",
        )
        .into_compile_error()
        .into();
    }
    let implementation = parse_macro_input!(item as ItemImpl);
    expand_plugin_impl(implementation)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[allow(clippy::too_many_lines)]
fn expand_plugin_impl(mut implementation: ItemImpl) -> syn::Result<proc_macro2::TokenStream> {
    if implementation.trait_.is_some() || !implementation.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &implementation,
            "#[plugin_impl] requires a non-generic inherent impl",
        ));
    }
    let Type::Path(plugin_path) = implementation.self_ty.as_ref() else {
        return Err(syn::Error::new_spanned(
            &implementation.self_ty,
            "Plugin implementation type must be a path",
        ));
    };
    let plugin_type = &plugin_path.path;
    let plugin_ident = plugin_type
        .segments
        .last()
        .expect("type paths are non-empty")
        .ident
        .clone();
    let inputs_name = format_ident!("__LensoInputs{plugin_ident}");
    let module_name = format_ident!(
        "__lenso_custom_construction_{}",
        snake(&plugin_ident.to_string())
    );
    let sdk = authoring_crate();
    let mut create = None;
    let mut stop = None;
    for item in &mut implementation.items {
        let syn::ImplItem::Fn(method) = item else {
            continue;
        };
        let is_create = take_marker(&mut method.attrs, "create");
        let is_stop = take_marker(&mut method.attrs, "stop");
        let marked_method = method.clone();
        for input in &mut method.sig.inputs {
            if let syn::FnArg::Typed(argument) = input {
                argument
                    .attrs
                    .retain(|attribute| !attribute.path().is_ident("lifecycle"));
            }
        }
        if is_create && is_stop {
            return Err(syn::Error::new_spanned(
                method,
                "one method cannot be both create and stop",
            ));
        }
        if is_create && create.replace(marked_method.clone()).is_some() {
            return Err(syn::Error::new_spanned(
                method,
                "duplicate #[create] method",
            ));
        }
        if is_stop && stop.replace(marked_method).is_some() {
            return Err(syn::Error::new_spanned(method, "duplicate #[stop] method"));
        }
    }
    if create.is_none() && stop.is_none() {
        return Ok(quote!(#implementation));
    }

    let construct_body = if let Some(create) = &create {
        expand_create_call(create, plugin_type, &inputs_name, &sdk)?
    } else {
        quote!(super::#plugin_type::__lenso_auto_construct(context))
    };
    let (stop_function, stop_entry) = if let Some(stop) = &stop {
        let body = expand_stop_call(stop, plugin_type, &sdk)?;
        (
            quote! {
                fn stop(
                    object: ::std::rc::Rc<dyn ::std::any::Any>,
                    lifecycle: #sdk::__private::LifecycleContext,
                ) -> #sdk::__private::PluginFuture {
                    let object = match object.downcast::<super::#plugin_type>() {
                        Ok(object) => object,
                        Err(_) => return Box::pin(async {
                            Err(#sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                                detail: "linked Plugin stop hook received the wrong type".to_owned(),
                            })
                        }),
                    };
                    #body
                }
            },
            quote!(Some(stop)),
        )
    } else {
        (quote!(), quote!(None))
    };

    Ok(quote! {
        #implementation

        #[doc(hidden)]
        mod #module_name {
            const _: () = assert!(
                super::#plugin_type::__LENSO_AUTHORING_VERSION == 2,
                "#[plugin_impl] create/stop hooks cannot be combined with legacy lifecycle, Port, resources, or tasks fields",
            );

            fn plugin_type() -> ::std::any::TypeId {
                ::std::any::TypeId::of::<super::#plugin_type>()
            }

            fn construct(
                context: #sdk::__private::ConstructionContext,
            ) -> #sdk::__private::ErasedConstructionFuture {
                #construct_body
            }

            #stop_function

            #sdk::__private::__inventory::submit! {
                #sdk::__private::LinkedPluginConstruction::new(
                    plugin_type,
                    true,
                    construct,
                    #stop_entry,
                )
            }
        }
    })
}

fn expand_create_call(
    method: &syn::ImplItemFn,
    plugin_type: &Path,
    inputs_name: &syn::Ident,
    sdk: &proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    let mut names = Vec::new();
    let mut arguments = Vec::new();
    for input in &method.sig.inputs {
        let syn::FnArg::Typed(argument) = input else {
            return Err(syn::Error::new_spanned(
                input,
                "#[create] is an associated function without a receiver",
            ));
        };
        let syn::Pat::Ident(pattern) = argument.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &argument.pat,
                "#[create] inputs must be plain identifiers",
            ));
        };
        let lifecycle = argument
            .attrs
            .iter()
            .any(|attribute| attribute.path().is_ident("lifecycle"));
        if lifecycle {
            if !is_named_type(&argument.ty, "LifecycleContext") {
                return Err(syn::Error::new_spanned(
                    &argument.ty,
                    "#[lifecycle] input must have type LifecycleContext",
                ));
            }
            arguments.push(quote!(context.lifecycle().clone()));
        } else {
            names.push(pattern.ident.clone());
            arguments.push(quote!(#pattern));
        }
    }
    let method_name = &method.sig.ident;
    let invoke = if method.sig.asyncness.is_some() {
        quote!(super::#plugin_type::#method_name(#(#arguments),*).await)
    } else {
        quote!(super::#plugin_type::#method_name(#(#arguments),*))
    };
    let value = if returns_result(&method.sig.output) {
        quote!(#invoke.map_err(|error| #sdk::__private::RuntimeFailure::PluginFailure {
            detail: format!("Plugin construction failed: {error}"),
        })?)
    } else {
        invoke
    };
    Ok(quote! {
        Box::pin(async move {
            let super::#inputs_name { #(#names),* } =
                super::#plugin_type::__lenso_inputs(&context)?;
            let plugin = #value;
            Ok(::std::rc::Rc::new(plugin) as ::std::rc::Rc<dyn ::std::any::Any>)
        })
    })
}

fn expand_stop_call(
    method: &syn::ImplItemFn,
    plugin_type: &Path,
    sdk: &proc_macro2::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    let mut inputs = method.sig.inputs.iter();
    let Some(syn::FnArg::Receiver(receiver)) = inputs.next() else {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[stop] requires an &self receiver",
        ));
    };
    if receiver.reference.is_none() || receiver.mutability.is_some() {
        return Err(syn::Error::new_spanned(receiver, "#[stop] requires &self"));
    }
    let mut arguments = Vec::new();
    for input in inputs {
        let syn::FnArg::Typed(argument) = input else {
            return Err(syn::Error::new_spanned(input, "invalid #[stop] input"));
        };
        if !argument
            .attrs
            .iter()
            .any(|attribute| attribute.path().is_ident("lifecycle"))
            || !is_named_type(&argument.ty, "LifecycleContext")
        {
            return Err(syn::Error::new_spanned(
                input,
                "#[stop] accepts only an optional #[lifecycle] LifecycleContext",
            ));
        }
        arguments.push(quote!(lifecycle));
    }
    if arguments.len() > 1 {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[stop] accepts at most one lifecycle input",
        ));
    }
    let method_name = &method.sig.ident;
    let invoke = if method.sig.asyncness.is_some() {
        quote!(super::#plugin_type::#method_name(object.as_ref(), #(#arguments),*).await)
    } else {
        quote!(super::#plugin_type::#method_name(object.as_ref(), #(#arguments),*))
    };
    let result = if returns_result(&method.sig.output) {
        quote!(#invoke.map_err(|error| #sdk::__private::RuntimeFailure::PluginFailure {
            detail: format!("Plugin stop failed: {error}"),
        }))
    } else {
        quote!({ #invoke; Ok(()) })
    };
    Ok(quote!(Box::pin(async move { #result })))
}

fn returns_result(output: &syn::ReturnType) -> bool {
    let syn::ReturnType::Type(_, ty) = output else {
        return false;
    };
    let Type::Path(path) = ty.as_ref() else {
        return false;
    };
    path.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "Result")
}

fn expand_authoring_item(attributes: TokenStream, item: TokenStream) -> TokenStream {
    let attributes = match syn::parse::<PluginAttributes>(attributes) {
        Ok(attributes) => attributes,
        Err(error) => return authoring_error(&error),
    };
    let item = match syn::parse::<Item>(item) {
        Ok(item) => item,
        Err(error) => return authoring_error(&error),
    };
    let expanded = match item {
        Item::Fn(function) => expand_plugin_function(&attributes, &function),
        Item::Struct(plugin) => expand_plugin_struct(&attributes, plugin),
        other => Err(syn::Error::new_spanned(
            other,
            "a native Plugin must be declared by a factory function or a named-field struct",
        )),
    };
    match expanded {
        Ok(tokens) => tokens.into(),
        Err(error) => authoring_error(&error),
    }
}

fn authoring_error(error: &syn::Error) -> TokenStream {
    syn::Error::new(error.span(), error.to_string())
        .into_compile_error()
        .into()
}

/// Derives the locked JSON Schema fragment consumed by a struct-level Plugin.
#[proc_macro_derive(PluginConfig, attributes(lenso, serde))]
pub fn plugin_config(item: TokenStream) -> TokenStream {
    let input = match syn::parse::<DeriveInput>(item) {
        Ok(input) => input,
        Err(error) => return authoring_error(&error),
    };
    match expand_plugin_config(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => authoring_error(&error),
    }
}

fn expand_plugin_config(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "Plugin configuration must be a named-field struct",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &data.fields,
            "Plugin configuration must use named fields",
        ));
    };
    let mut properties = Map::new();
    let mut defaults = Map::new();
    let mut required = Vec::new();
    for field in &fields.named {
        let ident = field.ident.as_ref().expect("named fields have identifiers");
        let name = serde_field_name(&field.attrs, ident)?;
        let (schema, optional) = configuration_type_schema(&field.ty)?;
        if let Some(default) = configuration_field_default(&field.attrs)? {
            if !configuration_value_matches_schema(&default, &schema) {
                return Err(syn::Error::new_spanned(
                    field,
                    "Plugin configuration default does not match the field type",
                ));
            }
            defaults.insert(name.clone(), default);
        }
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
    let defaults = canonical_json(&Value::Object(defaults));
    let macro_name = format_ident!("__lenso_config_schema_{}", snake(&input.ident.to_string()));
    let defaults_macro_name = format_ident!(
        "__lenso_config_defaults_{}",
        snake(&input.ident.to_string())
    );
    Ok(quote! {
        #[doc(hidden)]
        #[macro_export]
        macro_rules! #macro_name {
            () => { #schema };
        }
        #[doc(hidden)]
        #[macro_export]
        macro_rules! #defaults_macro_name {
            () => { #defaults };
        }
    })
}

fn configuration_field_default(attributes: &[Attribute]) -> syn::Result<Option<Value>> {
    let mut default = None;
    for attribute in attributes {
        if !attribute.path().is_ident("lenso") {
            continue;
        }
        attribute.parse_nested_meta(|meta| {
            if !meta.path.is_ident("default") {
                return Err(meta.error("expected `default = <JSON literal>`"));
            }
            if default.is_some() {
                return Err(meta.error("duplicate Plugin configuration default"));
            }
            let expression = meta.value()?.parse::<Expr>()?;
            let encoded = quote!(#expression).to_string();
            default = Some(serde_json::from_str(&encoded).map_err(|error| {
                meta.error(format!(
                    "Plugin configuration default must be a JSON literal: {error}"
                ))
            })?);
            Ok(())
        })?;
    }
    Ok(default)
}

fn configuration_value_matches_schema(value: &Value, schema: &Value) -> bool {
    match schema.get("type").and_then(Value::as_str) {
        Some("array") => value.as_array().is_some_and(|items| {
            schema.get("items").is_some_and(|schema| {
                items
                    .iter()
                    .all(|item| configuration_value_matches_schema(item, schema))
            })
        }),
        Some("boolean") => value.is_boolean(),
        Some("integer") => value
            .as_number()
            .is_some_and(|number| number.is_i64() || number.is_u64()),
        Some("number") => value.is_number(),
        Some("string") => value.is_string(),
        _ => false,
    }
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
            "Plugin configuration fields must use portable named types",
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
                "unsupported Plugin configuration field type; use String, bool, a number, Option<T>, or Vec<T>",
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

#[allow(clippy::too_many_lines)]
fn expand_plugin_function(
    attributes: &PluginAttributes,
    function: &ItemFn,
) -> syn::Result<proc_macro2::TokenStream> {
    let sdk = authoring_crate();
    if attributes.descriptor.is_none()
        && (attributes.configuration_schema.is_some()
            || attributes.configuration_defaults.is_some())
    {
        return Err(syn::Error::new_spanned(
            function,
            "`configuration_schema` and `configuration_defaults` require `descriptor` on a factory function",
        ));
    }
    if attributes.validate.is_some()
        || attributes.prepare.is_some()
        || attributes.activate.is_some()
        || attributes.deactivate.is_some()
        || attributes.lifecycle
        || attributes.consumer
    {
        return Err(syn::Error::new_spanned(
            function,
            "struct-level Plugin attributes are unavailable on factory functions",
        ));
    }
    let (plugin_id, root_slot) = plugin_metadata()?;
    let descriptor_json = attributes
        .descriptor
        .as_ref()
        .map(|descriptor| {
            plugin_descriptor(
                &plugin_id,
                &root_slot,
                descriptor,
                attributes.configuration_schema.as_ref(),
                attributes.configuration_defaults.as_ref(),
            )
        })
        .transpose()?;
    let function_name = &function.sig.ident;
    let generated_plugin = format_ident!("__lenso_plugin_{function_name}");
    let link_function = format_ident!("__lenso_link_{function_name}");
    let descriptor_constant = descriptor_json.map(|descriptor| {
        let artifact =
            format!("LENSO_PLUGIN_DESCRIPTOR_V1\0{descriptor}\0END_LENSO_PLUGIN_DESCRIPTOR_V1");
        let artifact_length = artifact.len();
        let artifact = proc_macro2::Literal::byte_string(artifact.as_bytes());
        let package_file_tracking = package_file_tracking([
            attributes.configuration_schema.as_ref(),
            attributes.configuration_defaults.as_ref(),
        ]);
        quote! {
            /// Generated package-owned Plugin Descriptor bytes.
            pub const PLUGIN_DESCRIPTOR_JSON: &str = #descriptor;
            /// Linker-retained descriptor artifact consumed without executing package code.
            #[doc(hidden)]
            #[used]
            pub static __LENSO_PLUGIN_DESCRIPTOR_ARTIFACT: [u8; #artifact_length] = *#artifact;
            #(#package_file_tracking)*
        }
    });

    Ok(quote! {
        /// Runtime package identity derived from Cargo package metadata.
        pub const PACKAGE_ID: &str = #plugin_id;
        /// Exact linked Cargo package version.
        pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
        /// Exact Host Build identity for this linked package.
        pub const FACTORY_IDENTITY: &str = concat!(#plugin_id, "@", env!("CARGO_PKG_VERSION"));
        #descriptor_constant

        #function

        #[doc(hidden)]
        mod #generated_plugin {
            #[derive(Clone, Copy, Debug, Default)]
            struct Factory;

            impl #sdk::__private::NativePluginFactory for Factory {
                fn package_id(&self) -> &'static str {
                    #plugin_id
                }

                fn package_version(&self) -> &'static str {
                    env!("CARGO_PKG_VERSION")
                }

                fn instantiate(
                    &self,
                    context: #sdk::__private::NativePluginFactoryContext<'_>,
                ) -> Result<
                    #sdk::__private::NativePluginInstance,
                    #sdk::__private::RuntimeFailure,
                > {
                    super::#function_name(context)
                }
            }

            pub(super) fn factory() -> std::rc::Rc<dyn #sdk::__private::NativePluginFactory> {
                std::rc::Rc::new(Factory)
            }

            #sdk::__private::__inventory::submit! {
                #sdk::__private::LinkedNativePluginFactory::new(
                    factory,
                    super::PLUGIN_DESCRIPTOR_JSON,
                )
            }

            // Make Cargo track the manifest that supplied the generated identity.
            const _: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        }

        /// Explicit Host linkage anchor generated for this Plugin factory.
        #[doc(hidden)]
        pub fn #link_function() {
            #sdk::__private::link_native_plugin(
                #sdk::__private::LinkedNativePluginFactory::new(
                    #generated_plugin::factory,
                    PLUGIN_DESCRIPTOR_JSON,
                ),
            );
        }
    })
}

/// Derives one or more provided Capability endpoints, one native factory, and static registration.
///
/// Apply this to one inherent implementation containing the Capabilities' domain methods
/// for a struct already annotated with [`plugin`]. Generated bindings lower the
/// methods into the Adapter-facing Provider trait. Existing explicit Provider
/// trait implementations remain supported as a single-Capability compatibility
/// escape hatch. Multi-Capability Plugins must use one inherent implementation.
#[proc_macro_attribute]
pub fn provides(attributes: TokenStream, item: TokenStream) -> TokenStream {
    let capabilities =
        parse_macro_input!(attributes with Punctuated::<Path, Token![,]>::parse_terminated);
    let implementation = parse_macro_input!(item as ItemImpl);
    expand_provides(
        &capabilities.into_iter().collect::<Vec<_>>(),
        &implementation,
    )
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}

struct CapabilityContribution {
    namespace: Path,
    descriptor: syn::Ident,
    endpoints: syn::Ident,
    lower: syn::Ident,
    object_lower: syn::Ident,
    trait_object_lower: syn::Ident,
    provider_wrapper: syn::Ident,
    projection_module: syn::Ident,
}

fn capability_contributions(capabilities: &[Path]) -> syn::Result<Vec<CapabilityContribution>> {
    let mut seen = BTreeSet::new();
    capabilities
        .iter()
        .enumerate()
        .map(|(index, capability)| {
            let path = quote!(#capability).to_string();
            if !seen.insert(path) {
                return Err(syn::Error::new_spanned(
                    capability,
                    "a Plugin cannot provide the same Capability more than once",
                ));
            }
            let mut namespace = capability.clone();
            let capability_ident = namespace
                .segments
                .pop()
                .ok_or_else(|| syn::Error::new_spanned(capability, "Capability path is empty"))?
                .into_value()
                .ident;
            namespace.segments.pop_punct();
            if namespace.segments.is_empty() {
                return Err(syn::Error::new_spanned(
                    capability,
                    "Capability must be namespace-qualified, for example `agent::Agent`",
                ));
            }
            let capability_snake = snake(&capability_ident.to_string());
            Ok(CapabilityContribution {
                namespace,
                descriptor: format_ident!("__lenso_provided_{capability_snake}"),
                endpoints: format_ident!("__lenso_native_endpoints_{capability_snake}"),
                lower: format_ident!("__lenso_native_lower_{capability_snake}"),
                object_lower: format_ident!("__lenso_native_lower_object_{capability_snake}"),
                trait_object_lower: format_ident!(
                    "__lenso_native_lower_trait_object_{capability_snake}"
                ),
                provider_wrapper: format_ident!("Provider"),
                projection_module: format_ident!("projection_{index}"),
            })
        })
        .collect()
}

fn provided_module(
    capabilities: &[Path],
    implementation: &ItemImpl,
) -> syn::Result<(syn::Ident, bool)> {
    if capabilities.is_empty() {
        return Err(syn::Error::new_spanned(
            implementation,
            "`provides` requires at least one namespace-qualified Capability",
        ));
    }
    if capabilities.len() > 1 && implementation.trait_.is_some() {
        return Err(syn::Error::new_spanned(
            implementation,
            "multiple Capabilities require one inherent impl containing their domain methods",
        ));
    }
    let Type::Path(plugin_type) = implementation.self_ty.as_ref() else {
        return Err(syn::Error::new_spanned(
            &implementation.self_ty,
            "the Plugin provider type must be a path",
        ));
    };
    let plugin_ident = plugin_type
        .path
        .segments
        .last()
        .ok_or_else(|| {
            syn::Error::new_spanned(&plugin_type.path, "the Plugin provider type is empty")
        })?
        .ident
        .clone();
    Ok((plugin_ident, implementation.trait_.is_none()))
}

#[allow(clippy::too_many_lines)]
fn expand_provides(
    capabilities: &[Path],
    implementation: &ItemImpl,
) -> syn::Result<proc_macro2::TokenStream> {
    let sdk = authoring_crate();
    let (plugin_ident, lowers_domain_methods) = provided_module(capabilities, implementation)?;
    let contributions = capability_contributions(capabilities)?;
    let provided_descriptors = contributions
        .iter()
        .map(|contribution| {
            let namespace = &contribution.namespace;
            let descriptor = &contribution.descriptor;
            quote!(#namespace::#descriptor!())
        })
        .collect::<Vec<_>>();
    let plugin_descriptor = format_ident!(
        "__lenso_plugin_descriptor_{}",
        snake(&plugin_ident.to_string())
    );
    let generated_plugin = format_ident!("__lenso_provider_{}", snake(&plugin_ident.to_string()));
    let link_function = format_ident!("__lenso_link_{}", snake(&plugin_ident.to_string()));
    let lifecycle = format_ident!("__LensoLifecycle{plugin_ident}");
    let artifact = format_ident!("__LENSO_PLUGIN_DESCRIPTOR_ARTIFACT_{plugin_ident}");
    let provider_implementations = contributions
        .iter()
        .filter_map(|contribution| {
            let namespace = &contribution.namespace;
            let lower = &contribution.lower;
            lowers_domain_methods.then(|| {
                quote! {
                    #namespace::#lower!(#plugin_ident, #sdk::__private);
                }
            })
        })
        .collect::<Vec<_>>();
    let object_provider_implementations = contributions
        .iter()
        .map(|contribution| {
            let namespace = &contribution.namespace;
            let provider_wrapper = &contribution.provider_wrapper;
            let projection_module = &contribution.projection_module;
            let object_lower = if lowers_domain_methods {
                &contribution.object_lower
            } else {
                &contribution.trait_object_lower
            };
            quote! {
                mod #projection_module {
                    #[derive(Clone, Debug)]
                    pub(super) struct #provider_wrapper(
                        pub(super) #sdk::__private::PluginObject<super::super::#plugin_ident>
                    );

                    impl #provider_wrapper {
                        fn get(
                            &self,
                        ) -> Result<
                            ::std::rc::Rc<super::super::#plugin_ident>,
                            #sdk::__private::RuntimeFailure,
                        > {
                            self.0.get()
                        }
                    }

                    super::super::#namespace::#object_lower!(
                        #provider_wrapper,
                        super::super::#plugin_ident,
                        #sdk::__private
                    );
                }
            }
        })
        .collect::<Vec<_>>();
    let endpoint_contributions = contributions
        .iter()
        .map(|contribution| {
            let namespace = &contribution.namespace;
            let endpoints = &contribution.endpoints;
            let provider_wrapper = &contribution.provider_wrapper;
            let projection_module = &contribution.projection_module;
            quote! {
                let (provided_requests, provided_streams, provided_events) =
                    super::#namespace::#endpoints!(
                        #projection_module::#provider_wrapper(plugin.clone()),
                        #sdk::__private
                    );
                request_endpoints.extend(provided_requests);
                stream_endpoints.extend(provided_streams);
                event_endpoints.extend(provided_events);
            }
        })
        .collect::<Vec<_>>();
    let v2_endpoint_contributions = endpoint_contributions.clone();

    let mut implementation = implementation.clone();
    implementation
        .attrs
        .push(syn::parse_quote!(#[allow(clippy::unused_async, clippy::unused_async_trait_impl)]));

    Ok(quote! {
        #implementation
        #(#provider_implementations)*

        /// Generated package-owned Plugin Descriptor bytes.
        pub const PLUGIN_DESCRIPTOR_JSON: &str = #plugin_descriptor!(
            #(#provided_descriptors),*
        );
        #[doc(hidden)]
        const __LENSO_PLUGIN_DESCRIPTOR_ARTIFACT_TEXT: &str = concat!(
            "LENSO_PLUGIN_DESCRIPTOR_V1\0",
            #plugin_descriptor!(#(#provided_descriptors),*),
            "\0END_LENSO_PLUGIN_DESCRIPTOR_V1",
        );
        /// Linker-retained descriptor artifact consumed without executing package code.
        #[doc(hidden)]
        #[used]
        pub static #artifact: &[u8] = __LENSO_PLUGIN_DESCRIPTOR_ARTIFACT_TEXT.as_bytes();

        #[doc(hidden)]
        mod #generated_plugin {
            #(#object_provider_implementations)*

            #[derive(Clone, Copy, Debug, Default)]
            struct Factory;

            impl #sdk::__private::NativePluginFactory for Factory {
                fn package_id(&self) -> &'static str { super::PACKAGE_ID }
                fn package_version(&self) -> &'static str { super::PACKAGE_VERSION }
                fn runtime_profile(&self) -> &'static str {
                    super::#plugin_ident::__LENSO_RUNTIME_PROFILE
                }

                fn instantiate(
                    &self,
                    context: #sdk::__private::NativePluginFactoryContext<'_>,
                ) -> Result<
                    #sdk::__private::NativePluginInstance,
                    #sdk::__private::RuntimeFailure,
                > {
                    if super::#plugin_ident::__LENSO_AUTHORING_VERSION == 2 {
                        let plugin = #sdk::__private::PluginObject::<super::#plugin_ident>::empty();
                        let lifecycle = #sdk::__private::CompleteObjectLifecycle::linked(
                            plugin.clone(),
                            context.configuration(),
                        )?;
                        let mut request_endpoints = Vec::new();
                        let mut stream_endpoints = Vec::new();
                        let mut event_endpoints = Vec::new();
                        #(#v2_endpoint_contributions)*
                        return Ok(#sdk::__private::NativePluginInstance::with_all_endpoints(
                            request_endpoints,
                            stream_endpoints,
                            event_endpoints,
                            lifecycle,
                        ));
                    }
                    let plugin = ::std::rc::Rc::new(
                        super::#plugin_ident::__lenso_construct(context)?,
                    );
                    let lifecycle = super::#lifecycle { plugin: plugin.clone() };
                    let plugin = #sdk::__private::PluginObject::from_value(plugin);
                    let mut request_endpoints = Vec::new();
                    let mut stream_endpoints = Vec::new();
                    let mut event_endpoints = Vec::new();
                    #(#endpoint_contributions)*
                    Ok(#sdk::__private::NativePluginInstance::with_all_endpoints(
                        request_endpoints,
                        stream_endpoints,
                        event_endpoints,
                        lifecycle,
                    ))
                }
            }

            pub(super) fn factory() -> ::std::rc::Rc<dyn #sdk::__private::NativePluginFactory> {
                ::std::rc::Rc::new(Factory)
            }

            #sdk::__private::__inventory::submit! {
                #sdk::__private::LinkedNativePluginFactory::new(
                    factory,
                    super::PLUGIN_DESCRIPTOR_JSON,
                )
            }
        }

        /// Explicit Host linkage anchor generated for this Plugin factory.
        #[doc(hidden)]
        pub fn #link_function() {
            #sdk::__private::link_native_plugin(
                #sdk::__private::LinkedNativePluginFactory::new(
                    #generated_plugin::factory,
                    PLUGIN_DESCRIPTOR_JSON,
                ),
            );
        }
    })
}

#[allow(clippy::too_many_lines)]
fn expand_plugin_struct(
    attributes: &PluginAttributes,
    mut plugin: ItemStruct,
) -> syn::Result<proc_macro2::TokenStream> {
    let sdk = authoring_crate();
    if attributes.descriptor.is_some() {
        return Err(syn::Error::new_spanned(
            &plugin.ident,
            "struct-level Plugins derive their Descriptor; remove `descriptor`",
        ));
    }
    let (plugin_id, root_slot) = plugin_metadata()?;
    let package_version = env::var("CARGO_PKG_VERSION").map_err(|_| {
        syn::Error::new_spanned(
            &plugin.ident,
            "CARGO_PKG_VERSION is unavailable while deriving Plugin Descriptor",
        )
    })?;
    let StructFields {
        config_type,
        ports,
        tasks,
        initializers,
        construction_fields,
    } = analyze_struct_fields(&mut plugin, &sdk)?;
    let schema = configuration_schema_tokens(
        attributes.configuration_schema.as_ref(),
        config_type.as_ref(),
    )?;
    let configuration_defaults = configuration_defaults_tokens(
        attributes.configuration_schema.as_ref(),
        attributes.configuration_defaults.as_ref(),
        config_type.as_ref(),
    )?;
    let name = &plugin.ident;
    let inputs_name = format_ident!("__LensoInputs{name}");
    let input_fields = construction_fields
        .iter()
        .filter_map(|field| match field.kind {
            ConstructionFieldKind::Config | ConstructionFieldKind::Dependency { .. } => {
                let name = &field.name;
                let ty = &field.ty;
                Some(quote!(#name: #ty))
            }
            ConstructionFieldKind::Private | ConstructionFieldKind::Legacy => None,
        });
    let input_initializers = construction_fields
        .iter()
        .filter_map(|field| v2_input_initializer(field, &sdk));
    let construction_module = format_ident!("__lenso_construction_{}", snake(&name.to_string()));
    let v2_configuration = construct_v2_configuration(
        &plugin_id,
        config_type.as_ref(),
        attributes.validate.as_ref(),
        &sdk,
    );
    let v2_initializers = construction_fields
        .iter()
        .map(|field| v2_field_initializer(field, &sdk))
        .collect::<Vec<_>>();
    let uses_legacy_authoring = construction_fields
        .iter()
        .any(|field| matches!(field.kind, ConstructionFieldKind::Legacy))
        || attributes.lifecycle
        || attributes.prepare.is_some()
        || attributes.activate.is_some()
        || attributes.deactivate.is_some();
    let authoring_version = if uses_legacy_authoring { 1_u32 } else { 2_u32 };
    let runtime_profile = if uses_legacy_authoring {
        "lenso.native-authoring@1"
    } else {
        "lenso.native-authoring@2"
    };
    let v2_construct = if uses_legacy_authoring {
        quote! {
            Err(#sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                detail: "legacy Plugin fields cannot use authoring version 2".to_owned(),
            })
        }
    } else {
        quote! {
            #v2_configuration
            let plugin = Self { #(#v2_initializers),* };
            Ok(::std::rc::Rc::new(plugin) as ::std::rc::Rc<dyn ::std::any::Any>)
        }
    };
    let lifecycle_name = format_ident!("__LensoLifecycle{name}");
    let descriptor_macro = format_ident!("__lenso_plugin_descriptor_{}", snake(&name.to_string()));
    let requirement_macros = ports
        .iter()
        .map(|(_, client, cardinality)| requirement_macro(client, *cardinality))
        .collect::<syn::Result<Vec<_>>>()?;
    let dependency_requirement_macros = construction_fields
        .iter()
        .filter_map(|field| match &field.kind {
            ConstructionFieldKind::Dependency {
                id,
                client,
                cardinality,
            } => Some(named_requirement_macro(client, *cardinality, id)),
            ConstructionFieldKind::Config
            | ConstructionFieldKind::Private
            | ConstructionFieldKind::Legacy => None,
        })
        .collect::<syn::Result<Vec<_>>>()?;
    let connect_ports = ports.iter().map(|(field, _, _)| {
        quote! { self.plugin.#field.connect(context.dependencies())?; }
    });
    let connect_tasks = task_connectors(&tasks);
    let requirement_parts = intersperse_commas(
        requirement_macros
            .into_iter()
            .chain(dependency_requirement_macros)
            .collect(),
    );
    let (prefix, after_schema, suffix, defaults) = descriptor_affixes(
        &plugin_id,
        &package_version,
        &root_slot,
        authoring_version,
        runtime_profile,
    );
    let construct_configuration = if let Some(config_type) = &config_type {
        let validate = attributes
            .validate
            .as_ref()
            .map(|path| quote!(#path(&configuration)?;));
        quote! {
            let configuration = #sdk::__private::serde_json::from_str::<#config_type>(context.configuration())
                .map_err(|error| #sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("invalid {} configuration: {error}", #plugin_id),
                })?;
            #validate
        }
    } else {
        if attributes.configuration_schema.is_some() {
            return Err(syn::Error::new_spanned(
                &plugin.ident,
                "`configuration_schema` requires a `#[config]` field",
            ));
        }
        if attributes.validate.is_some() {
            return Err(syn::Error::new_spanned(
                &plugin.ident,
                "`validate` requires a `#[config]` field",
            ));
        }
        if attributes.configuration_defaults.is_some() {
            return Err(syn::Error::new_spanned(
                &plugin.ident,
                "`configuration_defaults` requires a `#[config]` field",
            ));
        }
        quote! {
            let configuration = #sdk::__private::serde_json::from_str::<#sdk::__private::serde_json::Value>(context.configuration())
                .map_err(|error| #sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("invalid {} configuration: {error}", #plugin_id),
                })?;
            if !configuration.as_object().is_some_and(|object| object.is_empty()) {
                return Err(#sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("{} does not accept configuration", #plugin_id),
                });
            }
        }
    };
    if attributes.lifecycle
        && (attributes.prepare.is_some()
            || attributes.activate.is_some()
            || attributes.deactivate.is_some())
    {
        return Err(syn::Error::new_spanned(
            &plugin.ident,
            "`lifecycle` replaces the `prepare`, `activate`, and `deactivate` function attributes",
        ));
    }
    let prepare = if attributes.lifecycle {
        quote! {
            let plugin = self.plugin.clone();
            Box::pin(async move { #sdk::Lifecycle::prepare(plugin.as_ref(), context).await })
        }
    } else {
        hook(attributes.prepare.as_ref(), &sdk)
    };
    let activate_hook = if attributes.lifecycle {
        quote! {
            let plugin = self.plugin.clone();
            Box::pin(async move { #sdk::Lifecycle::activate(plugin.as_ref(), context).await })
        }
    } else {
        hook(attributes.activate.as_ref(), &sdk)
    };
    let deactivate_hook = if attributes.lifecycle {
        quote! {
            let plugin = self.plugin.clone();
            Box::pin(async move { #sdk::Lifecycle::deactivate(plugin.as_ref(), context).await })
        }
    } else {
        hook(attributes.deactivate.as_ref(), &sdk)
    };
    let disconnect_tasks = task_disconnectors(&tasks);
    let activate = if tasks.is_empty() {
        activate_hook
    } else {
        quote! {
            let plugin = self.plugin.clone();
            let activation = { #activate_hook };
            Box::pin(async move {
                let result = activation.await;
                if result.is_err() {
                    #(#disconnect_tasks)*
                }
                result
            })
        }
    };
    let disconnect_tasks = task_disconnectors(&tasks);
    let deactivate = if tasks.is_empty() {
        deactivate_hook
    } else {
        quote! {
            let plugin = self.plugin.clone();
            #(#disconnect_tasks)*
            let deactivation = { #deactivate_hook };
            deactivation
        }
    };
    let package_file_tracking = package_file_tracking([
        attributes.configuration_schema.as_ref(),
        attributes.configuration_defaults.as_ref(),
    ]);
    let consumer_finalizer = if attributes.consumer {
        let generated_plugin = format_ident!("__lenso_consumer_{}", snake(&name.to_string()));
        let link_function = format_ident!("__lenso_link_{}", snake(&name.to_string()));
        let artifact = format_ident!("__LENSO_PLUGIN_DESCRIPTOR_ARTIFACT_{name}");
        Some(quote! {
            /// Generated package-owned Plugin Descriptor bytes.
            pub const PLUGIN_DESCRIPTOR_JSON: &str = #descriptor_macro!();
            #[doc(hidden)]
            const __LENSO_PLUGIN_DESCRIPTOR_ARTIFACT_TEXT: &str = concat!(
                "LENSO_PLUGIN_DESCRIPTOR_V1\0",
                #descriptor_macro!(),
                "\0END_LENSO_PLUGIN_DESCRIPTOR_V1",
            );
            /// Linker-retained descriptor artifact consumed without executing package code.
            #[doc(hidden)]
            #[used]
            pub static #artifact: &[u8] = __LENSO_PLUGIN_DESCRIPTOR_ARTIFACT_TEXT.as_bytes();

            #[doc(hidden)]
            mod #generated_plugin {
                #[derive(Clone, Copy, Debug, Default)]
                struct Factory;

                impl #sdk::__private::NativePluginFactory for Factory {
                    fn package_id(&self) -> &'static str { super::PACKAGE_ID }
                    fn package_version(&self) -> &'static str { super::PACKAGE_VERSION }
                    fn runtime_profile(&self) -> &'static str {
                        super::#name::__LENSO_RUNTIME_PROFILE
                    }

                    fn instantiate(
                        &self,
                        context: #sdk::__private::NativePluginFactoryContext<'_>,
                    ) -> Result<
                        #sdk::__private::NativePluginInstance,
                        #sdk::__private::RuntimeFailure,
                    > {
                        if super::#name::__LENSO_AUTHORING_VERSION == 2 {
                            let object = #sdk::__private::PluginObject::<super::#name>::empty();
                            let lifecycle = #sdk::__private::CompleteObjectLifecycle::linked(
                                object,
                                context.configuration(),
                            )?;
                            return Ok(#sdk::__private::NativePluginInstance::with_lifecycle(
                                Vec::new(),
                                lifecycle,
                            ));
                        }
                        let plugin = ::std::rc::Rc::new(super::#name::__lenso_construct(context)?);
                        let lifecycle = super::#lifecycle_name { plugin };
                        Ok(#sdk::__private::NativePluginInstance::with_lifecycle(
                            Vec::new(),
                            lifecycle,
                        ))
                    }
                }

                pub(super) fn factory() -> ::std::rc::Rc<dyn #sdk::__private::NativePluginFactory> {
                    ::std::rc::Rc::new(Factory)
                }

                #sdk::__private::__inventory::submit! {
                    #sdk::__private::LinkedNativePluginFactory::new(
                        factory,
                        super::PLUGIN_DESCRIPTOR_JSON,
                    )
                }
            }

            /// Explicit Host linkage anchor generated for this Plugin factory.
            #[doc(hidden)]
            pub fn #link_function() {
                #sdk::__private::link_native_plugin(
                    #sdk::__private::LinkedNativePluginFactory::new(
                        #generated_plugin::factory,
                        PLUGIN_DESCRIPTOR_JSON,
                    ),
                );
            }
        })
    } else {
        None
    };

    Ok(quote! {
        /// Runtime package identity derived from Cargo package metadata.
        pub const PACKAGE_ID: &str = #plugin_id;
        /// Exact linked Cargo package version.
        pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
        /// Exact Host Build identity for this linked package.
        pub const FACTORY_IDENTITY: &str = concat!(#plugin_id, "@", env!("CARGO_PKG_VERSION"));

        #plugin

        #[doc(hidden)]
        struct #inputs_name {
            #(#input_fields),*
        }

        #[doc(hidden)]
        macro_rules! #descriptor_macro {
            () => {
                concat!(#prefix, #schema, ",\"configuration_defaults\":", #configuration_defaults, #after_schema, #suffix #(, #requirement_parts)*, #defaults)
            };
            ($first:expr $(, $rest:expr)*) => {
                concat!(#prefix, #schema, ",\"configuration_defaults\":", #configuration_defaults, #after_schema, $first $(, ",", $rest)*, #suffix #(, #requirement_parts)*, #defaults)
            };
        }

        impl #name {
            #[doc(hidden)]
            const __LENSO_AUTHORING_VERSION: u32 = #authoring_version;
            #[doc(hidden)]
            const __LENSO_RUNTIME_PROFILE: &'static str = #runtime_profile;

            #[doc(hidden)]
            fn __lenso_inputs(
                context: &#sdk::__private::ConstructionContext,
            ) -> Result<#inputs_name, #sdk::__private::RuntimeFailure> {
                #v2_configuration
                Ok(#inputs_name { #(#input_initializers),* })
            }

            #[doc(hidden)]
            #[allow(unreachable_code)]
            fn __lenso_construct(
                context: #sdk::__private::NativePluginFactoryContext<'_>,
            ) -> Result<Self, #sdk::__private::RuntimeFailure> {
                if context.entrypoint() != "default" {
                    return Err(#sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                        detail: format!("unsupported {} entrypoint {}", #plugin_id, context.entrypoint()),
                    });
                }
                #construct_configuration
                Ok(Self { #(#initializers),* })
            }

            #[doc(hidden)]
            fn __lenso_auto_construct(
                context: #sdk::__private::ConstructionContext,
            ) -> #sdk::__private::ErasedConstructionFuture {
                Box::pin(async move {
                    let _ = context;
                    #v2_construct
                })
            }
        }

        #[doc(hidden)]
        mod #construction_module {
            fn plugin_type() -> ::std::any::TypeId {
                ::std::any::TypeId::of::<super::#name>()
            }

            #sdk::__private::__inventory::submit! {
                #sdk::__private::LinkedPluginConstruction::new(
                    plugin_type,
                    false,
                    super::#name::__lenso_auto_construct,
                    None,
                )
            }
        }

        #[doc(hidden)]
        #[derive(Clone, Debug)]
        struct #lifecycle_name {
            plugin: ::std::rc::Rc<#name>,
        }

        impl #sdk::__private::PluginLifecycle for #lifecycle_name {
            fn prepare(&self, context: #sdk::__private::PrepareContext) -> #sdk::__private::PluginFuture {
                #prepare
            }

            fn activate(&self, context: #sdk::__private::ActivateContext) -> #sdk::__private::PluginFuture {
                let connected = (|| -> Result<(), #sdk::__private::RuntimeFailure> {
                    #(#connect_ports)*
                    #(#connect_tasks)*
                    Ok(())
                })();
                if let Err(error) = connected {
                    return Box::pin(#sdk::__private::futures::future::ready(Err(error)));
                }
                #activate
            }

            fn deactivate(&self, context: #sdk::__private::DeactivateContext) -> #sdk::__private::PluginFuture {
                #deactivate
            }
        }

        const _: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        #(#package_file_tracking)*

        #consumer_finalizer
    })
}

fn descriptor_affixes(
    plugin_id: &str,
    package_version: &str,
    root_slot: &str,
    authoring_version: u32,
    runtime_profile: &str,
) -> (String, &'static str, &'static str, &'static str) {
    let prefix = format!(
        "{{\"authoring_version\":{authoring_version},\"runtime_profile\":{},\"plugin_id\":{},\"release_version\":{},\"root_slot\":{},\"runtime_package_id\":{},\"runtime_package_revision\":{},\"entrypoint\":\"default\",\"configuration_schema\":",
        serde_json::to_string(runtime_profile).expect("runtime profile serializes"),
        serde_json::to_string(plugin_id).expect("Plugin ID serializes"),
        serde_json::to_string(package_version).expect("package version serializes"),
        serde_json::to_string(root_slot).expect("root Slot serializes"),
        serde_json::to_string(plugin_id).expect("runtime package ID serializes"),
        serde_json::to_string(package_version).expect("package version serializes"),
    );
    let after_schema = ",\"provided_capabilities\":[";
    let suffix = "],\"required_capabilities\":[";
    let defaults = "],\"execution_class\":\"lenso.native-rust@1\",\"restart_policy\":{\"mode\":\"never\",\"max_attempts\":0,\"window\":{\"secs\":0,\"nanos\":0},\"backoff\":{\"secs\":0,\"nanos\":0},\"stability\":{\"secs\":0,\"nanos\":0},\"jitter\":{\"secs\":0,\"nanos\":0}},\"criticality\":\"non_critical\"}";
    (prefix, after_schema, suffix, defaults)
}

fn package_file_tracking<'a>(
    paths: impl IntoIterator<Item = Option<&'a LitStr>>,
) -> Vec<proc_macro2::TokenStream> {
    paths
        .into_iter()
        .flatten()
        .map(|path| {
            quote!(
                const _: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", #path));
            )
        })
        .collect()
}

fn configuration_schema_tokens(
    schema_path: Option<&LitStr>,
    config_type: Option<&Type>,
) -> syn::Result<proc_macro2::TokenStream> {
    if let Some(path) = schema_path {
        let schema = canonical_json(&read_configuration_schema(path)?);
        return Ok(quote!(#schema));
    }
    let Some(config_type) = config_type else {
        let schema = canonical_json(&json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "required": [],
            "properties": {},
        }));
        return Ok(quote!(#schema));
    };
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

fn configuration_defaults_tokens(
    schema_path: Option<&LitStr>,
    defaults_path: Option<&LitStr>,
    config_type: Option<&Type>,
) -> syn::Result<proc_macro2::TokenStream> {
    if let Some(path) = defaults_path {
        if schema_path.is_none() {
            return Err(syn::Error::new(
                path.span(),
                "`configuration_defaults` requires an explicit `configuration_schema`",
            ));
        }
        let defaults = read_configuration_defaults(path)?;
        let schema = read_configuration_schema(schema_path.expect("checked above"))?;
        validate_configuration_defaults(&defaults, &schema).map_err(|detail| {
            syn::Error::new(
                path.span(),
                format!("invalid package configuration defaults: {detail}"),
            )
        })?;
        let defaults = canonical_json(&defaults);
        return Ok(quote!(#defaults));
    }
    if schema_path.is_some() || config_type.is_none() {
        let defaults = canonical_json(&json!({}));
        return Ok(quote!(#defaults));
    }
    let config_type = config_type.expect("checked above");
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
    let macro_name = format_ident!(
        "__lenso_config_defaults_{}",
        snake(&config_name.to_string())
    );
    if namespace.segments.is_empty() {
        Ok(quote!(#macro_name!()))
    } else {
        Ok(quote!(#namespace::#macro_name!()))
    }
}

struct StructFields {
    config_type: Option<Type>,
    ports: Vec<(syn::Ident, Path, PortCardinality)>,
    tasks: Vec<syn::Ident>,
    initializers: Vec<proc_macro2::TokenStream>,
    construction_fields: Vec<ConstructionField>,
}

struct ConstructionField {
    name: syn::Ident,
    ty: Type,
    kind: ConstructionFieldKind,
}

enum ConstructionFieldKind {
    Config,
    Dependency {
        id: LitStr,
        client: Box<Type>,
        cardinality: DependencyCardinality,
    },
    Private,
    Legacy,
}

#[derive(Clone, Copy)]
enum DependencyCardinality {
    One,
    Optional,
    Many,
}

#[derive(Clone, Copy)]
enum PortCardinality {
    One,
    Many,
}

#[allow(clippy::too_many_lines)]
fn analyze_struct_fields(
    plugin: &mut ItemStruct,
    sdk: &proc_macro2::TokenStream,
) -> syn::Result<StructFields> {
    let Fields::Named(fields) = &mut plugin.fields else {
        return Err(syn::Error::new_spanned(
            &plugin.fields,
            "a struct-level Plugin requires named fields",
        ));
    };
    let mut config = None;
    let mut ports = Vec::new();
    let mut tasks = Vec::new();
    let mut resources = None;
    let mut initializers = Vec::new();
    let mut construction_fields = Vec::new();
    for field in &mut fields.named {
        let name = field.ident.as_ref().expect("named fields have identifiers");
        let is_config = take_marker(&mut field.attrs, "config");
        let is_tasks = take_marker(&mut field.attrs, "tasks");
        let is_resources = take_marker(&mut field.attrs, "resources");
        let dependency = take_dependency(&mut field.attrs)?;
        if usize::from(is_config)
            + usize::from(is_tasks)
            + usize::from(is_resources)
            + usize::from(dependency.is_some())
            > 1
        {
            return Err(syn::Error::new_spanned(
                field,
                "a Plugin field can have only one construction marker",
            ));
        }
        if is_config {
            if config.replace(field.ty.clone()).is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "a Plugin has exactly one `#[config]` field",
                ));
            }
            initializers.push(quote!(#name: configuration));
            construction_fields.push(ConstructionField {
                name: name.clone(),
                ty: field.ty.clone(),
                kind: ConstructionFieldKind::Config,
            });
        } else if let Some(id) = dependency {
            let (client, cardinality) = dependency_client(&field.ty)?;
            initializers.push(quote! {
                #name: return Err(#sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                    detail: concat!("dependency field `", stringify!(#name), "` requires authoring version 2").to_owned(),
                })
            });
            construction_fields.push(ConstructionField {
                name: name.clone(),
                ty: field.ty.clone(),
                kind: ConstructionFieldKind::Dependency {
                    id,
                    client: Box::new(client),
                    cardinality,
                },
            });
        } else if is_tasks {
            if !is_named_type(&field.ty, "ManagedTasks") {
                return Err(syn::Error::new_spanned(
                    &field.ty,
                    "a `#[tasks]` field must have type `ManagedTasks`",
                ));
            }
            if !tasks.is_empty() {
                return Err(syn::Error::new_spanned(
                    field,
                    "a Plugin has at most one `#[tasks]` field",
                ));
            }
            tasks.push(name.clone());
            initializers.push(quote!(#name: ::core::default::Default::default()));
            construction_fields.push(ConstructionField {
                name: name.clone(),
                ty: field.ty.clone(),
                kind: ConstructionFieldKind::Legacy,
            });
        } else if is_resources {
            if !is_named_type(&field.ty, "InstanceResources") {
                return Err(syn::Error::new_spanned(
                    &field.ty,
                    "a `#[resources]` field must have type `InstanceResources`",
                ));
            }
            if resources.replace(name.clone()).is_some() {
                return Err(syn::Error::new_spanned(
                    field,
                    "a Plugin has at most one `#[resources]` field",
                ));
            }
            initializers.push(quote!(#name: context.resources().clone()));
            construction_fields.push(ConstructionField {
                name: name.clone(),
                ty: field.ty.clone(),
                kind: ConstructionFieldKind::Legacy,
            });
        } else if let Some((client, cardinality)) = port_client(&field.ty)? {
            ports.push((name.clone(), client, cardinality));
            initializers.push(quote!(#name: ::core::default::Default::default()));
            construction_fields.push(ConstructionField {
                name: name.clone(),
                ty: field.ty.clone(),
                kind: ConstructionFieldKind::Legacy,
            });
        } else {
            initializers.push(legacy_default_initializer(name, &field.ty, sdk));
            construction_fields.push(ConstructionField {
                name: name.clone(),
                ty: field.ty.clone(),
                kind: ConstructionFieldKind::Private,
            });
        }
    }
    Ok(StructFields {
        config_type: config,
        ports,
        tasks,
        initializers,
        construction_fields,
    })
}

fn take_dependency(attributes: &mut Vec<Attribute>) -> syn::Result<Option<LitStr>> {
    let mut id = None;
    let mut seen = false;
    let mut retained = Vec::with_capacity(attributes.len());
    for attribute in attributes.drain(..) {
        if !attribute.path().is_ident("dependency") {
            retained.push(attribute);
            continue;
        }
        if seen {
            return Err(syn::Error::new_spanned(
                attribute,
                "duplicate `dependency` marker",
            ));
        }
        seen = true;
        attribute.parse_nested_meta(|meta| {
            if !meta.path.is_ident("id") {
                return Err(meta.error("expected `id = \"public_requirement_id\"`"));
            }
            id = Some(meta.value()?.parse()?);
            Ok(())
        })?;
    }
    *attributes = retained;
    if seen {
        id.map(Some).ok_or_else(|| {
            syn::Error::new(proc_macro2::Span::call_site(), "dependency id is required")
        })
    } else {
        Ok(None)
    }
}

fn dependency_client(ty: &Type) -> syn::Result<(Type, DependencyCardinality)> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "dependency type must be a generated client",
        ));
    };
    let segment = path.path.segments.last().expect("type paths are non-empty");
    if segment.ident == "Option" {
        return Ok((
            single_type_argument(segment, ty)?.clone(),
            DependencyCardinality::Optional,
        ));
    }
    if segment.ident == "Vec" {
        let bound = single_type_argument(segment, ty)?;
        let Type::Path(bound_path) = bound else {
            return Err(syn::Error::new_spanned(
                bound,
                "many dependency must contain a generated client",
            ));
        };
        let bound_segment = bound_path
            .path
            .segments
            .last()
            .expect("type paths are non-empty");
        if bound_segment.ident != "BoundCapabilityClient" {
            return Err(syn::Error::new_spanned(
                bound,
                "many dependency must be `Vec<BoundCapabilityClient<Client>>`",
            ));
        }
        return Ok((
            single_type_argument(bound_segment, bound)?.clone(),
            DependencyCardinality::Many,
        ));
    }
    Ok((ty.clone(), DependencyCardinality::One))
}

fn single_type_argument<'a>(segment: &'a syn::PathSegment, ty: &Type) -> syn::Result<&'a Type> {
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "dependency wrapper requires one type",
        ));
    };
    let [GenericArgument::Type(inner)] = arguments.args.iter().collect::<Vec<_>>().as_slice()
    else {
        return Err(syn::Error::new_spanned(
            ty,
            "dependency wrapper requires one type",
        ));
    };
    Ok(inner)
}

fn legacy_default_initializer(
    name: &syn::Ident,
    ty: &Type,
    sdk: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    quote! {
        #name: {
            trait __LensoMaybeDefault<T> {
                fn __lenso_default(self) -> Option<T>;
            }
            impl<T: Default> __LensoMaybeDefault<T> for &&::std::marker::PhantomData<T> {
                fn __lenso_default(self) -> Option<T> {
                    Some(T::default())
                }
            }
            impl<T> __LensoMaybeDefault<T> for &::std::marker::PhantomData<T> {
                fn __lenso_default(self) -> Option<T> {
                    None
                }
            }
            let marker = ::std::marker::PhantomData::<#ty>;
            (&&marker).__lenso_default().ok_or_else(|| {
                #sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                    detail: concat!(
                        "Plugin field `",
                        stringify!(#name),
                        "` has no default; use authoring version 2 with #[create]",
                    )
                    .to_owned(),
                }
            })?
        }
    }
}

fn construct_v2_configuration(
    plugin_id: &str,
    config_type: Option<&Type>,
    validate: Option<&Path>,
    sdk: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    if let Some(config_type) = config_type {
        let validate = validate.map(|path| quote!(#path(&configuration)?;));
        quote! {
            let configuration = #sdk::__private::serde_json::from_str::<#config_type>(
                context.configuration(),
            )
            .map_err(|error| #sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                detail: format!("invalid {} configuration: {error}", #plugin_id),
            })?;
            #validate
        }
    } else {
        quote! {
            let configuration = #sdk::__private::serde_json::from_str::<
                #sdk::__private::serde_json::Value,
            >(context.configuration())
            .map_err(|error| #sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                detail: format!("invalid {} configuration: {error}", #plugin_id),
            })?;
            if !configuration.as_object().is_some_and(|object| object.is_empty()) {
                return Err(#sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("{} does not accept configuration", #plugin_id),
                });
            }
        }
    }
}

fn v2_field_initializer(
    field: &ConstructionField,
    sdk: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    let name = &field.name;
    let ty = &field.ty;
    match &field.kind {
        ConstructionFieldKind::Config => quote!(#name: configuration),
        ConstructionFieldKind::Dependency {
            id,
            client,
            cardinality: DependencyCardinality::One,
        } => quote! {
            #name: {
                let dependency = context.dependencies().requirement(#id)?;
                <#client as #sdk::__private::CapabilityClient>::from_dependencies(&dependency)?
            }
        },
        ConstructionFieldKind::Dependency {
            id,
            client,
            cardinality: DependencyCardinality::Optional,
        } => quote! {
            #name: {
                let dependency = context.dependencies().requirement(#id)?;
                if dependency.bindings().is_empty() {
                    None
                } else {
                    Some(<#client as #sdk::__private::CapabilityClient>::from_dependencies(
                        &dependency,
                    )?)
                }
            }
        },
        ConstructionFieldKind::Dependency {
            id,
            client,
            cardinality: DependencyCardinality::Many,
        } => quote! {
            #name: {
                let dependency = context.dependencies().requirement(#id)?;
                <#client as #sdk::__private::CapabilityClientMany>::many_from_dependencies(
                    &dependency,
                )?
            }
        },
        ConstructionFieldKind::Private => quote! {
            #name: {
                trait __LensoMaybeDefault<T> {
                    fn __lenso_default(self) -> Option<T>;
                }
                impl<T: Default> __LensoMaybeDefault<T> for &&::std::marker::PhantomData<T> {
                    fn __lenso_default(self) -> Option<T> {
                        Some(T::default())
                    }
                }
                impl<T> __LensoMaybeDefault<T> for &::std::marker::PhantomData<T> {
                    fn __lenso_default(self) -> Option<T> {
                        None
                    }
                }
                let marker = ::std::marker::PhantomData::<#ty>;
                (&&marker).__lenso_default().ok_or_else(|| {
                    #sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                        detail: concat!(
                            "Plugin field `",
                            stringify!(#name),
                            "` has no default; add a #[create] constructor",
                        )
                        .to_owned(),
                    }
                })?
            }
        },
        ConstructionFieldKind::Legacy => quote! {
            #name: return Err(#sdk::__private::RuntimeFailure::InvalidResolvedPlan {
                detail: concat!(
                    "legacy Plugin field `",
                    stringify!(#name),
                    "` cannot use authoring version 2",
                )
                .to_owned(),
            })
        },
    }
}

fn v2_input_initializer(
    field: &ConstructionField,
    sdk: &proc_macro2::TokenStream,
) -> Option<proc_macro2::TokenStream> {
    let name = &field.name;
    match &field.kind {
        ConstructionFieldKind::Config => Some(quote!(#name: configuration)),
        ConstructionFieldKind::Dependency {
            id,
            client,
            cardinality: DependencyCardinality::One,
        } => Some(quote! {
            #name: {
                let dependency = context.dependencies().requirement(#id)?;
                <#client as #sdk::__private::CapabilityClient>::from_dependencies(&dependency)?
            }
        }),
        ConstructionFieldKind::Dependency {
            id,
            client,
            cardinality: DependencyCardinality::Optional,
        } => Some(quote! {
            #name: {
                let dependency = context.dependencies().requirement(#id)?;
                if dependency.bindings().is_empty() {
                    None
                } else {
                    Some(<#client as #sdk::__private::CapabilityClient>::from_dependencies(
                        &dependency,
                    )?)
                }
            }
        }),
        ConstructionFieldKind::Dependency {
            id,
            client,
            cardinality: DependencyCardinality::Many,
        } => Some(quote! {
            #name: {
                let dependency = context.dependencies().requirement(#id)?;
                <#client as #sdk::__private::CapabilityClientMany>::many_from_dependencies(
                    &dependency,
                )?
            }
        }),
        ConstructionFieldKind::Private | ConstructionFieldKind::Legacy => None,
    }
}

fn is_named_type(ty: &Type, expected: &str) -> bool {
    let Type::Path(path) = ty else {
        return false;
    };
    path.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == expected && segment.arguments.is_empty())
}

fn take_marker(attributes: &mut Vec<Attribute>, name: &str) -> bool {
    let present = attributes
        .iter()
        .any(|attribute| attribute.path().is_ident(name));
    attributes.retain(|attribute| !attribute.path().is_ident(name));
    present
}

fn task_connectors(tasks: &[syn::Ident]) -> Vec<proc_macro2::TokenStream> {
    tasks
        .iter()
        .map(|field| {
            quote! { self.plugin.#field.__lenso_connect(context.tasks().clone())?; }
        })
        .collect()
}

fn task_disconnectors(tasks: &[syn::Ident]) -> Vec<proc_macro2::TokenStream> {
    tasks
        .iter()
        .map(|field| quote! { plugin.#field.__lenso_disconnect(); })
        .collect()
}

fn port_client(ty: &Type) -> syn::Result<Option<(Path, PortCardinality)>> {
    let Type::Path(path) = ty else {
        return Ok(None);
    };
    let Some(segment) = path.path.segments.last() else {
        return Ok(None);
    };
    let cardinality = if segment.ident == "Port" {
        PortCardinality::One
    } else if segment.ident == "ManyPort" {
        PortCardinality::Many
    } else {
        return Ok(None);
    };
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "Port or ManyPort requires one Capability client type",
        ));
    };
    let Some(syn::GenericArgument::Type(Type::Path(client))) = arguments.args.first() else {
        return Err(syn::Error::new_spanned(
            ty,
            "Port or ManyPort requires one Capability client type",
        ));
    };
    if arguments.args.len() != 1 {
        return Err(syn::Error::new_spanned(
            ty,
            "Port or ManyPort requires one Capability client type",
        ));
    }
    Ok(Some((client.path.clone(), cardinality)))
}

fn requirement_macro(
    client: &Path,
    cardinality: PortCardinality,
) -> syn::Result<proc_macro2::TokenStream> {
    let prefix = match cardinality {
        PortCardinality::One => "__lenso_required_",
        PortCardinality::Many => "__lenso_required_many_",
    };
    requirement_macro_path(client, prefix, None)
}

fn named_requirement_macro(
    client: &Type,
    cardinality: DependencyCardinality,
    requirement_id: &LitStr,
) -> syn::Result<proc_macro2::TokenStream> {
    let Type::Path(client) = client else {
        return Err(syn::Error::new_spanned(
            client,
            "dependency client must be a namespace-qualified generated client",
        ));
    };
    let prefix = match cardinality {
        DependencyCardinality::One => "__lenso_required_",
        DependencyCardinality::Optional => "__lenso_required_optional_",
        DependencyCardinality::Many => "__lenso_required_many_",
    };
    requirement_macro_path(&client.path, prefix, Some(requirement_id))
}

fn requirement_macro_path(
    client: &Path,
    prefix: &str,
    requirement_id: Option<&LitStr>,
) -> syn::Result<proc_macro2::TokenStream> {
    if client.segments.len() < 2 {
        return Err(syn::Error::new_spanned(
            client,
            "a Capability client must be namespace-qualified, for example `model::ModelClient`",
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
    let macro_name = format_ident!("{}{}", prefix, snake(&client_name.to_string()));
    Ok(requirement_id.map_or_else(
        || quote!(#namespace::#macro_name!()),
        |requirement_id| quote!(#namespace::#macro_name!(#requirement_id)),
    ))
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

fn hook(path: Option<&Path>, sdk: &proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    path.map_or_else(
        || quote!(Box::pin(#sdk::__private::futures::future::ready(Ok(())))),
        |path| quote!(#path(&self.plugin, &context)),
    )
}

fn canonical_json(value: &Value) -> String {
    serde_json::to_string(value).expect("JSON values serialize")
}

fn authoring_crate() -> proc_macro2::TokenStream {
    for package in ["lenso", "lenso-native-adapter"] {
        match crate_name(package) {
            Ok(FoundCrate::Itself) => {
                let ident = format_ident!("{}", package.replace('-', "_"));
                return quote!(::#ident);
            }
            Ok(FoundCrate::Name(name)) => {
                let ident = format_ident!("{name}");
                return quote!(::#ident);
            }
            Err(_) => {}
        }
    }
    quote!(::lenso_native_adapter)
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

fn plugin_descriptor(
    plugin_id: &str,
    root_slot: &str,
    descriptor: &LitStr,
    configuration_schema: Option<&LitStr>,
    configuration_defaults: Option<&LitStr>,
) -> syn::Result<String> {
    let supplied: Value = serde_json::from_str(&descriptor.value()).map_err(|error| {
        syn::Error::new(
            descriptor.span(),
            format!("Plugin Descriptor input is not valid JSON: {error}"),
        )
    })?;
    let mut supplied = supplied.as_object().cloned().ok_or_else(|| {
        syn::Error::new(
            descriptor.span(),
            "Plugin Descriptor input must be an object",
        )
    })?;
    if supplied.contains_key("configuration_schema") {
        return Err(syn::Error::new(
            descriptor.span(),
            "Plugin Descriptor input cannot contain `configuration_schema`; use the package-owned schema path attribute",
        ));
    }
    if supplied.contains_key("configuration_defaults") {
        return Err(syn::Error::new(
            descriptor.span(),
            "Plugin Descriptor input cannot contain `configuration_defaults`; use the package-owned defaults path attribute",
        ));
    }
    if let Some(schema_path) = configuration_schema {
        supplied.insert(
            "configuration_schema".to_owned(),
            read_configuration_schema(schema_path)?,
        );
    }
    if let Some(defaults_path) = configuration_defaults {
        if configuration_schema.is_none() {
            return Err(syn::Error::new(
                defaults_path.span(),
                "`configuration_defaults` requires `configuration_schema`",
            ));
        }
        let defaults = read_configuration_defaults(defaults_path)?;
        let schema = supplied
            .get("configuration_schema")
            .expect("explicit configuration Schema was inserted above");
        validate_configuration_defaults(&defaults, schema).map_err(|detail| {
            syn::Error::new(
                defaults_path.span(),
                format!("invalid package configuration defaults: {detail}"),
            )
        })?;
        supplied.insert("configuration_defaults".to_owned(), defaults);
    }
    for owned in [
        "plugin_id",
        "release_version",
        "root_slot",
        "runtime_package_id",
        "runtime_package_revision",
        "entrypoint",
        "execution_class",
        "restart_policy",
        "criticality",
    ] {
        if supplied.contains_key(owned) {
            return Err(syn::Error::new(
                descriptor.span(),
                format!("Plugin Descriptor input cannot override generated field `{owned}`"),
            ));
        }
    }
    let package_version = env::var("CARGO_PKG_VERSION").map_err(|_| {
        syn::Error::new(
            descriptor.span(),
            "CARGO_PKG_VERSION is unavailable while deriving Plugin Descriptor",
        )
    })?;
    Ok(complete_plugin_descriptor(
        plugin_id,
        &package_version,
        root_slot,
        supplied,
    ))
}

fn read_configuration_schema(schema_path: &LitStr) -> syn::Result<Value> {
    let schema = read_package_json(schema_path, "configuration Schema")?;
    if !schema.is_object() {
        return Err(syn::Error::new(
            schema_path.span(),
            "configuration Schema must be a JSON object",
        ));
    }
    Ok(schema)
}

fn read_configuration_defaults(defaults_path: &LitStr) -> syn::Result<Value> {
    let defaults = read_package_json(defaults_path, "configuration defaults")?;
    if !defaults.is_object() {
        return Err(syn::Error::new(
            defaults_path.span(),
            "configuration defaults must be a JSON object",
        ));
    }
    Ok(defaults)
}

fn read_package_json(path: &LitStr, label: &str) -> syn::Result<Value> {
    let relative = PathBuf::from(path.value());
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(syn::Error::new(
            path.span(),
            format!("{label} path must stay inside the Plugin package"),
        ));
    }
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| {
        syn::Error::new(
            path.span(),
            format!("CARGO_MANIFEST_DIR is unavailable while deriving {label}"),
        )
    })?;
    let full_path = PathBuf::from(manifest_dir).join(relative);
    let bytes = fs::read(&full_path).map_err(|error| {
        syn::Error::new(
            path.span(),
            format!("failed to read {label} {}: {error}", full_path.display()),
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        syn::Error::new(
            path.span(),
            format!("{label} {} is invalid JSON: {error}", full_path.display()),
        )
    })
}

fn validate_configuration_defaults(defaults: &Value, schema: &Value) -> Result<(), String> {
    if !defaults.is_object() {
        return Err("$: defaults must be an object".to_owned());
    }
    validate_default_value(defaults, schema, "$")
}

fn validate_default_value(value: &Value, schema: &Value, path: &str) -> Result<(), String> {
    let schema = schema
        .as_object()
        .ok_or_else(|| format!("{path}: configuration Schema must be an object"))?;
    if schema
        .get("x-lenso-sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(format!(
            "{path}: sensitive configuration cannot have a package default"
        ));
    }
    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        let valid = match expected {
            "array" => value.is_array(),
            "boolean" => value.is_boolean(),
            "integer" => value
                .as_number()
                .is_some_and(|number| number.is_i64() || number.is_u64()),
            "null" => value.is_null(),
            "number" => value.is_number(),
            "object" => value.is_object(),
            "string" => value.is_string(),
            _ => false,
        };
        if !valid {
            return Err(format!(
                "{path}: default does not match Schema type `{expected}`"
            ));
        }
    }
    if let (Some(minimum), Some(number)) = (schema.get("minimum"), value.as_f64()) {
        let minimum = minimum
            .as_f64()
            .ok_or_else(|| format!("{path}: Schema minimum must be a number"))?;
        if number < minimum {
            return Err(format!(
                "{path}: default must be greater than or equal to {minimum}"
            ));
        }
    }
    if let Some(expected) = schema.get("const")
        && value != expected
    {
        return Err(format!("{path}: default does not match Schema const"));
    }
    if let Some(allowed) = schema.get("enum") {
        let allowed = allowed
            .as_array()
            .ok_or_else(|| format!("{path}: Schema enum must be an array"))?;
        if !allowed.contains(value) {
            return Err(format!("{path}: default is not in Schema enum"));
        }
    }
    validate_default_object(value, schema, path)?;
    validate_default_array(value, schema, path)
}

fn validate_default_object(
    value: &Value,
    schema: &Map<String, Value>,
    path: &str,
) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let empty = Map::new();
    let properties = schema.get("properties").map_or(Ok(&empty), |properties| {
        properties
            .as_object()
            .ok_or_else(|| format!("{path}: Schema properties must be an object"))
    })?;
    for (name, child) in object {
        if let Some(child_schema) = properties.get(name) {
            validate_default_value(child, child_schema, &format!("{path}.{name}"))?;
            continue;
        }
        match schema.get("additionalProperties") {
            Some(Value::Bool(false)) => {
                return Err(format!("{path}.{name}: additional property is not allowed"));
            }
            Some(Value::Object(additional_schema)) => validate_default_value(
                child,
                &Value::Object(additional_schema.clone()),
                &format!("{path}.{name}"),
            )?,
            _ => {}
        }
    }
    Ok(())
}

fn validate_default_array(
    value: &Value,
    schema: &Map<String, Value>,
    path: &str,
) -> Result<(), String> {
    let (Some(items), Some(item_schema)) = (value.as_array(), schema.get("items")) else {
        return Ok(());
    };
    for (index, item) in items.iter().enumerate() {
        validate_default_value(item, item_schema, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

fn complete_plugin_descriptor(
    plugin_id: &str,
    package_version: &str,
    root_slot: &str,
    mut supplied: Map<String, Value>,
) -> String {
    let mut generated = Map::new();
    generated.insert("plugin_id".to_owned(), json!(plugin_id));
    generated.insert("release_version".to_owned(), json!(package_version));
    generated.insert("root_slot".to_owned(), json!(root_slot));
    generated.insert("runtime_package_id".to_owned(), json!(plugin_id));
    generated.insert(
        "runtime_package_revision".to_owned(),
        json!(package_version),
    );
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
        .expect("generated Plugin Descriptor values must serialize")
}

fn plugin_metadata() -> syn::Result<(String, String)> {
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
    let lenso = manifest
        .get("package")
        .and_then(|package| package.get("metadata"))
        .and_then(|metadata| metadata.get("lenso"))
        .and_then(toml::Value::as_table)
        .ok_or_else(|| metadata_error("missing `[package.metadata.lenso]` in Cargo.toml"))?;
    let plugin_id = lenso
        .get("plugin-id")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| {
            metadata_error("missing `plugin-id = \"...\"` in `[package.metadata.lenso]`")
        })?;
    let root_slot = lenso
        .get("root-slot")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| {
            metadata_error("missing `root-slot = \"...\"` in `[package.metadata.lenso]`")
        })?;
    Ok((plugin_id.to_owned(), root_slot.to_owned()))
}

fn metadata_error(detail: &str) -> syn::Error {
    syn::Error::new(proc_macro2::Span::call_site(), detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn generated_descriptor_owns_identity_and_execution_defaults() {
        let supplied = serde_json::from_value::<Map<String, Value>>(json!({
            "provided_capabilities": [],
            "required_capabilities": []
        }))
        .unwrap();
        let descriptor = complete_plugin_descriptor("example.tool", "1.2.3", "tools", supplied);
        let descriptor: Value = serde_json::from_str(&descriptor).unwrap();

        assert_eq!(descriptor["plugin_id"], "example.tool");
        assert_eq!(descriptor["release_version"], "1.2.3");
        assert_eq!(descriptor["runtime_package_id"], "example.tool");
        assert_eq!(descriptor["runtime_package_revision"], "1.2.3");
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
        assert_eq!(schema["required"], json!(["name", "retries"]));
    }

    #[test]
    fn package_defaults_are_embedded_as_descriptor_data() {
        let path = LitStr::new(
            "tests/fixtures/config.defaults.json",
            proc_macro2::Span::call_site(),
        );
        let defaults = read_configuration_defaults(&path).unwrap();

        assert_eq!(defaults, json!({"name": "fixture", "retries": 3}));
    }

    #[test]
    fn factory_function_descriptor_embeds_package_defaults() {
        let descriptor = LitStr::new(
            r#"{"provided_capabilities":[],"required_capabilities":[]}"#,
            proc_macro2::Span::call_site(),
        );
        let schema = LitStr::new(
            "tests/fixtures/config.schema.json",
            proc_macro2::Span::call_site(),
        );
        let defaults = LitStr::new(
            "tests/fixtures/config.defaults.json",
            proc_macro2::Span::call_site(),
        );

        let generated = plugin_descriptor(
            "example.tool",
            "tools",
            &descriptor,
            Some(&schema),
            Some(&defaults),
        )
        .unwrap();
        let generated: Value = serde_json::from_str(&generated).unwrap();
        assert_eq!(
            generated["configuration_defaults"],
            json!({"name": "fixture", "retries": 3})
        );
    }

    #[test]
    fn typed_configuration_defaults_must_match_the_field_type() {
        let input: DeriveInput = parse_quote! {
            struct InvalidConfig {
                #[lenso(default = 3)]
                name: String,
            }
        };

        let error = expand_plugin_config(&input).unwrap_err();
        assert!(error.to_string().contains("does not match the field type"));
    }

    #[test]
    fn package_defaults_fail_closed_against_schema_constraints() {
        let schema = json!({
            "type": "object",
            "properties": {
                "retries": {"type": "integer", "minimum": 1},
                "token": {"x-lenso-sensitive": true}
            },
            "additionalProperties": false
        });

        assert_eq!(
            validate_configuration_defaults(&json!({"retries": 0}), &schema),
            Err("$.retries: default must be greater than or equal to 1".to_owned())
        );
        assert_eq!(
            validate_configuration_defaults(&json!({"token": {"secret_ref": "TOKEN"}}), &schema),
            Err("$.token: sensitive configuration cannot have a package default".to_owned())
        );
    }

    #[test]
    fn typed_ports_preserve_client_paths_and_cardinality() {
        let one: Type = parse_quote!(Port<secrets::SecretsClient>);
        let many: Type = parse_quote!(ManyPort<auth::AuthClient>);

        let (one_client, one_cardinality) = port_client(&one).unwrap().unwrap();
        let (many_client, many_cardinality) = port_client(&many).unwrap().unwrap();

        assert_eq!(quote!(#one_client).to_string(), "secrets :: SecretsClient");
        assert!(matches!(one_cardinality, PortCardinality::One));
        assert_eq!(quote!(#many_client).to_string(), "auth :: AuthClient");
        assert!(matches!(many_cardinality, PortCardinality::Many));
    }

    #[test]
    fn named_dependency_fields_determine_cardinality_without_type_only_matching() {
        let mut plugin: ItemStruct = parse_quote! {
            struct Consumer {
                #[dependency(id = "source")]
                source: store::StoreClient,
                #[dependency(id = "fallback")]
                fallback: Option<store::StoreClient>,
                #[dependency(id = "replicas")]
                replicas: Vec<BoundCapabilityClient<store::StoreClient>>,
            }
        };
        let fields = analyze_struct_fields(&mut plugin, &quote!(::lenso)).unwrap();

        assert_eq!(fields.construction_fields.len(), 3);
        let ids = fields
            .construction_fields
            .iter()
            .map(|field| match &field.kind {
                ConstructionFieldKind::Dependency {
                    id, cardinality, ..
                } => (
                    id.value(),
                    match cardinality {
                        DependencyCardinality::One => "one",
                        DependencyCardinality::Optional => "optional",
                        DependencyCardinality::Many => "many",
                    },
                ),
                _ => panic!("expected dependency field"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                ("source".to_owned(), "one"),
                ("fallback".to_owned(), "optional"),
                ("replicas".to_owned(), "many"),
            ]
        );
        assert!(plugin.fields.iter().all(|field| field.attrs.is_empty()));
    }

    #[test]
    fn managed_tasks_fields_are_initialized_and_connected_on_activate() {
        let mut plugin: ItemStruct = parse_quote! {
            struct Worker {
                #[tasks]
                tasks: ManagedTasks,
            }
        };
        let fields = analyze_struct_fields(&mut plugin, &quote!(::lenso)).unwrap();

        let task_field: syn::Ident = parse_quote!(tasks);
        assert_eq!(fields.tasks, vec![task_field]);
        assert_eq!(
            fields.initializers[0].to_string(),
            "tasks : :: core :: default :: Default :: default ()"
        );
        assert_eq!(
            task_connectors(&fields.tasks)[0].to_string(),
            "self . plugin . tasks . __lenso_connect (context . tasks () . clone ()) ? ;"
        );
        assert_eq!(
            task_disconnectors(&fields.tasks)[0].to_string(),
            "plugin . tasks . __lenso_disconnect () ;"
        );
        assert!(plugin.fields.iter().next().unwrap().attrs.is_empty());
    }

    #[test]
    fn multiple_capabilities_reject_trait_impls() {
        let implementation: ItemImpl = parse_quote! {
            impl fixture::Provider for ExamplePlugin {}
        };
        let error = expand_provides(
            &[parse_quote!(fixture::One), parse_quote!(fixture::Two)],
            &implementation,
        )
        .expect_err("multi-Capability authoring must have one inherent impl");

        assert!(
            error
                .to_string()
                .contains("multiple Capabilities require one inherent impl")
        );
    }

    #[test]
    fn duplicate_capabilities_are_rejected() {
        let implementation: ItemImpl = parse_quote! { impl ExamplePlugin {} };
        let error = expand_provides(
            &[parse_quote!(fixture::One), parse_quote!(fixture::One)],
            &implementation,
        )
        .expect_err("one Capability cannot be contributed twice");

        assert!(error.to_string().contains("same Capability more than once"));
    }

    #[test]
    fn capability_paths_must_be_namespace_qualified() {
        let implementation: ItemImpl = parse_quote! { impl ExamplePlugin {} };
        let error = expand_provides(&[parse_quote!(One)], &implementation)
            .expect_err("generated Capability macros live in their namespace");

        assert!(error.to_string().contains("namespace-qualified"));
    }
}
