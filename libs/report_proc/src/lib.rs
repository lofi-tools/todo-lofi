#![recursion_limit = "128"] // https://github.com/rust-lang/rust/issues/62059
extern crate proc_macro;
use proc_macro::TokenStream;
// use quote::quote;
use std::collections::BTreeSet;
use syn::spanned::Spanned;
// TODO use https://github.com/aacebo/zyn

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
    let item = syn::parse::<Item>(item)?;

    let f = match item {
        Item::Fn(f) => f,
        _ => {
            return Err(syn::Error::new(
                item.span(),
                "`#[snafu::report]` may only be used on functions",
            ));
        }
    };

    let ItemFn {
        attrs,
        vis,
        sig,
        block,
    } = f;

    let Signature {
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
        ReturnType::Default => quote! { () },
        ReturnType::Type(_, ty) => quote! { #ty },
    };

    let error_ty = quote! { <#output_ty as ::snafu::__InternalExtractErrorType>::Err };

    let output = quote! { -> ::snafu::Report<#error_ty> };

    let captured_original_body = if asyncness.is_some() {
        quote! { async #block.await }
    } else {
        quote! { (|| #block)() }
    };

    let ascribed_original_result = quote! {
        let __snafu_body: #output_ty = #captured_original_body;
    };

    let block = quote! {
        {
            #ascribed_original_result;
            <::snafu::Report<_> as ::core::convert::From<_>>::from(__snafu_body)
        }
    };

    Ok(quote! {
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

// use proc_macro::TokenStream;
// use std::collections::BTreeSet;
use zyn::quote::quote;
// use syn::{Item, ItemFn, ReturnType, Signature, spanned::Spanned};
use zyn::prelude::*;
use zyn::syn::{Item, ItemFn, ReturnType, Signature};

#[zyn::attribute]
pub fn zyn_report(#[zyn(input)] item: Item, _args: zyn::Args) -> zyn::TokenStream {
    let ItemFn {
        attrs,
        vis,
        sig,
        block,
        ..
    } = match item {
        Item::Fn(f) => f,
        _ => bail!("`#[snafu::report]` may only be used on functions"),
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

    zyn::zyn! {
        #(#attrs)*
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
    use crate::report::body;
    use zyn::prelude::*;
    use zyn::{
        Args,
        syn::{Item, ReturnType},
    }; // adjust path to your macro module

    #[test]
    fn transforms_sync_result_function() {
        let input: Item = zyn::parse! {
            fn parse_config(path: &str) -> Result<Config, ConfigError> {
                Ok(Config::default())
            }
        }
        .unwrap();

        let output = body(Args::empty(), input.into()).unwrap();

        zyn::assert_tokens_contain!(output, "-> ::snafu::Report<ConfigError>");
        zyn::assert_tokens_contain!(output, "let __snafu_body : Result < Config , ConfigError >");
        zyn::assert_tokens_contain!(output, "::core::convert::From<_>>::from(__snafu_body)");
    }

    #[test]
    fn transforms_async_result_function() {
        let input: Item = zyn::parse! {
            async fn fetch_user(id: u64) -> Result<User, ApiError> {
                api::get(id).await
            }
        }
        .unwrap();

        let output = body(Args::empty(), input.into()).unwrap();

        zyn::assert_tokens_contain!(output, "async fn fetch_user");
        zyn::assert_tokens_contain!(output, "-> ::snafu::Report<ApiError>");
        zyn::assert_tokens_contain!(output, "async { Ok ( Config :: default ( ) ) }.await");
    }

    #[test]
    fn transforms_function_with_unit_return() {
        let input: Item = zyn::parse! {
            fn log_startup() {
                println!("starting...");
            }
        }
        .unwrap();

        let output = body(Args::empty(), input.into()).unwrap();

        // Unit return -> extracts () as error type placeholder
        zyn::assert_tokens_contain!(output, "-> ::snafu::Report<()>");
        zyn::assert_tokens_contain!(output, "let __snafu_body : ()");
    }

    #[test]
    fn transforms_function_with_generic_error() {
        let input: Item = zyn::parse! {
            fn process<T: std::fmt::Debug>(val: T) -> Result<T, std::io::Error> {
                Ok(val)
            }
        }
        .unwrap();

        let output = body(Args::empty(), input.into()).unwrap();

        zyn::assert_tokens_contain!(output, "-> ::snafu::Report<std::io::Error>");
        zyn::assert_tokens_contain!(output, "let __snafu_body : Result < T , std::io::Error >");
    }

    #[test]
    fn rejects_non_function_items() {
        let input: Item = zyn::parse! {
            struct NotAFunction;
        }
        .unwrap();

        let result = body(Args::empty(), input.into());

        zyn::assert_diagnostic_error!(result, "may only be used on functions");
        zyn::assert_tokens_empty!(result.unwrap_err());
    }

    #[test]
    fn preserves_function_attributes_and_visibility() {
        let input: Item = zyn::parse! {
            #[deprecated(note = "use new_api")]
            #[doc(hidden)]
            pub(crate) fn legacy() -> Result<(), OldError> {
                Err(OldError)
            }
        }
        .unwrap();

        let output = body(Args::empty(), input.into()).unwrap();

        zyn::assert_tokens_contain!(output, "# [deprecated (note = \"use new_api\")]");
        zyn::assert_tokens_contain!(output, "# [doc (hidden)]");
        zyn::assert_tokens_contain!(output, "pub (crate) fn legacy");
        zyn::assert_tokens_contain!(output, "-> ::snafu::Report<OldError>");
    }

    #[test]
    fn preserves_const_unsafe_abi_modifiers() {
        let input: Item = zyn::parse! {
            pub const unsafe extern "C" fn raw_api() -> Result<i32, FfiError> {
                Ok(42)
            }
        }
        .unwrap();

        let output = body(Args::empty(), input.into()).unwrap();

        zyn::assert_tokens_contain!(output, "pub const unsafe extern \"C\" fn raw_api");
        zyn::assert_tokens_contain!(output, "-> ::snafu::Report<FfiError>");
    }

    #[test]
    fn handles_generics_and_where_clauses() {
        let input: Item = zyn::parse! {
            fn parse_json<'a, T: serde::Deserialize<'a>>(
                s: &'a str
            ) -> Result<T, serde_json::Error>
            where
                T: std::fmt::Debug,
            {
                serde_json::from_str(s)
            }
        }
        .unwrap();

        let output = body(Args::empty(), input.into()).unwrap();

        zyn::assert_tokens_contain!(
            output,
            "fn parse_json < 'a , T : serde::Deserialize < 'a > >"
        );
        zyn::assert_tokens_contain!(output, "where T : std::fmt::Debug ,");
        zyn::assert_tokens_contain!(output, "-> ::snafu::Report<serde_json::Error>");
    }
}
