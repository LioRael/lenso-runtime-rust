//! Authoring markers for portable Rust Lenso Plugins.

use proc_macro::TokenStream;

/// Marks the ordinary Plugin type used by product SDK authoring macros.
///
/// Portable execution has no native lifecycle to register, so the marker keeps
/// the type unchanged. Product SDKs generate the Capability dispatcher and the
/// portable Runtime SDK generates execution-target glue.
#[proc_macro_attribute]
pub fn plugin(attributes: TokenStream, item: TokenStream) -> TokenStream {
    if !attributes.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "portable #[lenso::plugin] does not accept lifecycle arguments",
        )
        .into_compile_error()
        .into();
    }
    item
}
