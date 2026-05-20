#![recursion_limit = "128"] // https://github.com/rust-lang/rust/issues/62059
extern crate proc_macro;
use proc_macro::TokenStream;
use syn::spanned::Spanned;

#[proc_macro_attribute]
pub fn report(attr: TokenStream, item: TokenStream) -> TokenStream {
    body(attr, item)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

fn body(
    _attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> syn::Result<proc_macro2::TokenStream> {
    let item = syn::parse::<syn::Item>(item)?;

    let f = match item {
        syn::Item::Fn(f) => f,
        _ => {
            return Err(syn::Error::new(
                item.span(),
                "`#[snafu::report]` may only be used on functions",
            ));
        }
    };

    let syn::ItemFn {
        attrs,
        vis,
        sig,
        block,
    } = f;

    let syn::Signature {
        constness,
        asyncness,
        unsafety,
        abi,
        fn_token,
        ident,
        generics,
        paren_token: _,
        inputs,
        variadic,
        output,
    } = sig;

    let output_ty = match output {
        syn::ReturnType::Default => quote::quote! { () },
        syn::ReturnType::Type(_, ty) => quote::quote! { #ty },
    };

    let error_ty = quote::quote! { <#output_ty as ::snafu::__InternalExtractErrorType>::Err };

    let output = quote::quote! { -> ::snafu::Report<#error_ty> };

    let captured_original_body = if asyncness.is_some() {
        quote::quote! { async #block.await }
    } else {
        quote::quote! { (|| #block)() }
    };

    let ascribed_original_result = quote::quote! {
        let __snafu_body: #output_ty = #captured_original_body;
    };

    let block = quote::quote! {
        {
            #ascribed_original_result;
            <::snafu::Report<_> as ::core::convert::From<_>>::from(__snafu_body)
        }
    };

    Ok(quote::quote! {
        #(#attrs)*
        #vis
        #constness
        #asyncness
        #unsafety
        #abi
        #fn_token
        #ident
        #generics
        (#inputs #variadic)
        #output
        #block
    })
}

// zyn-based implementation
use zyn::quote::quote;
use zyn::syn::{Item, ItemFn, ReturnType, Signature};

#[zyn::attribute]
pub fn zyn_report(#[zyn(input)] item: Item, args: zyn::Args) -> zyn::Output {
    zyn_body(item, args)
}
/// zyn-based body function for testing
fn zyn_body(item: Item, args: zyn::Args) -> zyn::Output {
    let ItemFn {
        attrs,
        vis,
        sig,
        block,
        ..
    } = match item {
        Item::Fn(f) => f,
        _ => {
            let diag = zyn::mark::error("`#[snafu::report]` may only be used on functions").build();
            return zyn::Output::from(diag);
        }
    };

    let Signature {
        constness,
        asyncness,
        unsafety,
        abi,
        fn_token,
        ident,
        generics,
        inputs,
        variadic,
        output,
        ..
    } = sig;

    let output_ty = match output {
        ReturnType::Default => quote! { () },
        ReturnType::Type(_, ty) => quote! { #ty },
    };

    let error_ty = quote! { <#output_ty as ::snafu::__InternalExtractErrorType>::Err };
    let captured_body = if asyncness.is_some() {
        quote! { async #block.await }
    } else {
        quote! { (|| #block)() }
    };

    let _ = args; // args unused for now
    zyn::zyn! {
        @for (attr in attrs.iter()) { #attr }
        #vis
        #constness #asyncness #unsafety #abi
        #fn_token #ident #generics
        (#inputs #variadic)
        -> ::snafu::Report<#error_ty>
        {
            let __snafu_body: #output_ty = #captured_body;
            <::snafu::Report<_> as ::core::convert::From<_>>::from(__snafu_body)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::zyn_body;
    use zyn::syn::Item;

    #[test]
    fn transforms_sync_result_function() {
        let input: Item = zyn::parse!("fn parse_config(path: &str) -> Result<Config, ConfigError> { Ok(Config::default()) }" => Item).unwrap();

        let output = zyn_body(input, zyn::Args::new());

        zyn::assert_tokens_contain!(output, "fn parse_config");
        zyn::assert_tokens_contain!(output, "__snafu_body");
        zyn::assert_tokens_contain!(output, "ConfigError");
    }

    #[test]
    fn transforms_async_result_function() {
        let input: Item = zyn::parse!("async fn fetch_user(id: u64) -> Result<User, ApiError> { api::get(id).await }" => Item).unwrap();

        let output = zyn_body(input, zyn::Args::new());

        zyn::assert_tokens_contain!(output, "async fn fetch_user");
        zyn::assert_tokens_contain!(output, "ApiError");
    }

    #[test]
    fn transforms_function_with_unit_return() {
        let input: Item =
            zyn::parse!("fn log_startup() { println!(\"starting...\"); }" => Item).unwrap();

        let output = zyn_body(input, zyn::Args::new());

        // Unit return -> extracts () as error type placeholder
        zyn::assert_tokens_contain!(output, "fn log_startup");
        zyn::assert_tokens_contain!(output, "let __snafu_body : ()");
    }

    #[test]
    fn transforms_function_with_generic_error() {
        let input: Item = zyn::parse!("fn process<T: std::fmt::Debug>(val: T) -> Result<T, std::io::Error> { Ok(val) }" => Item).unwrap();

        let output = zyn_body(input, zyn::Args::new());

        zyn::assert_tokens_contain!(output, "std :: io :: Error");
        zyn::assert_tokens_contain!(
            output,
            "let __snafu_body : Result < T , std :: io :: Error >"
        );
    }

    #[test]
    fn rejects_non_function_items() {
        let input: Item = zyn::parse!("struct NotAFunction;" => Item).unwrap();

        let output = zyn_body(input, zyn::Args::new());

        zyn::assert_diagnostic_error!(output, "may only be used on functions");
        zyn::assert_tokens_empty!(output.tokens());
    }

    #[test]
    fn preserves_function_attributes_and_visibility() {
        let input: Item = zyn::parse!("#[deprecated(note = \"use new_api\")] #[doc(hidden)] pub(crate) fn legacy() -> Result<(), OldError> { Err(OldError) }" => Item).unwrap();

        let output = zyn_body(input, zyn::Args::new());

        zyn::assert_tokens_contain!(output, "# [deprecated (note = \"use new_api\")]");
        zyn::assert_tokens_contain!(output, "# [doc (hidden)]");
        zyn::assert_tokens_contain!(output, "pub (crate) fn legacy");
        zyn::assert_tokens_contain!(output, "OldError");
    }

    #[test]
    fn preserves_const_unsafe_abi_modifiers() {
        let input: Item = zyn::parse!("pub const unsafe extern \"C\" fn raw_api() -> Result<i32, FfiError> { Ok(42) }" => Item).unwrap();

        let output = zyn_body(input, zyn::Args::new());

        zyn::assert_tokens_contain!(output, "pub const unsafe extern \"C\" fn raw_api");
        zyn::assert_tokens_contain!(output, "FfiError");
    }

    #[test]
    fn handles_generics_and_where_clauses() {
        let input: Item = zyn::parse!("fn parse_json<'a, T: serde::Deserialize<'a>>(s: &'a str) -> Result<T, serde_json::Error> where T: std::fmt::Debug, { serde_json::from_str(s) }" => Item).unwrap();

        let output = zyn_body(input, zyn::Args::new());

        zyn::assert_tokens_contain!(output, "fn parse_json");
        zyn::assert_tokens_contain!(output, "serde_json :: Error");
    }
}
