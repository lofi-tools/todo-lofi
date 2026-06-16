use zyn::syn;

#[derive(zyn::Attribute)]
#[zyn("entity_id")]
struct EntityIdConfig {
    #[zyn(default)]
    prefix: Option<String>,
}

fn entity_id_body(ident: &syn::Ident, prefix: &str) -> zyn::Output {
    zyn::zyn! {
        impl {{ ident }} {
            pub const PREFIX: &'static str = {{ prefix }};

            pub fn auto() -> Self {
                let millis = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("System time before UNIX epoch")
                    .as_millis() as u64;

                static COUNTER: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                let id = ((millis & 0xFFFF_FFFF) << 32) | (counter & 0xFFFF_FFFF);
                Self(id)
            }

            pub fn to_string_no_prefix(&self) -> String {
                self.0.to_string().to_lowercase()
            }
            pub fn strip_prefix(value: &str) -> &str {
                value.split('_').last().unwrap_or(value)
            }
        }

        #[automatically_derived]
        impl std::fmt::Display for {{ ident }} {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}_{}", Self::PREFIX, self.0)
            }
        }

        #[automatically_derived]
        impl std::str::FromStr for {{ ident }} {
            type Err = <u64 as std::str::FromStr>::Err;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let value = Self::strip_prefix(s);
                u64::from_str(&value).map(Self)
            }
        }

        #[automatically_derived]
        impl TryFrom<String> for {{ ident }} {
            type Error = <u64 as std::str::FromStr>::Err;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                value.parse()
            }
        }

        #[automatically_derived]
        impl TryFrom<&str> for {{ ident }} {
            type Error = <u64 as std::str::FromStr>::Err;
            fn try_from(value: &str) -> Result<Self, Self::Error> {
                value.parse()
            }
        }
    }
}

#[zyn::derive("EntityId", attributes(entity_id))]
pub fn entity_id(
    #[zyn(input)] ident: zyn::Extract<syn::Ident>,
    #[zyn(input)] config: zyn::Attr<EntityIdConfig>,
) -> zyn::Output {
    let prefix = config
        .prefix
        .clone()
        .unwrap_or_else(|| "entity".to_string());

    entity_id_body(&ident, &prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zyn::ToTokens;

    #[test]
    fn test_generate_with_prefix() {
        let ident = zyn::format_ident!("MyId");
        let output = entity_id_body(&ident, "test");
        zyn::assert_tokens_contain!(output, "PREFIX");
        zyn::assert_tokens_contain!(output, "Display for MyId");
    }

    #[test]
    fn test_generate_default_prefix() {
        let ident = zyn::format_ident!("MyId");
        let output = entity_id_body(&ident, "entity");
        zyn::assert_tokens_contain!(output, "PREFIX");
    }

    #[test]
    fn test_full_input_output() {
        let input: syn::DeriveInput = syn::parse_str("pub struct MyId(u64);").unwrap();

        let prefix = input
            .attrs
            .iter()
            .find_map(|attr| {
                if !attr.path().is_ident("entity_id") {
                    return None;
                }
                let args: zyn::Args = attr.parse_args().ok()?;
                args.get("prefix").and_then(|a| match a.as_expr() {
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) => Some(s.value()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| "entity".to_string());

        let output = entity_id_body(&input.ident, &prefix);
        let code = output.to_token_stream().to_string();

        assert!(code.contains("impl MyId"));
        assert!(code.contains("pub const PREFIX : & 'static str = \"entity\" ;"));
        assert!(code.contains("pub fn new () -> Self { let millis = std :: time :: SystemTime :: now () . duration_since (std :: time :: UNIX_EPOCH)"));
        assert!(code.contains("pub fn unprefixed (& self) -> String {"));
        assert!(code.contains("impl std :: fmt :: Display for MyId"));
        assert!(code.contains("impl std :: str :: FromStr for MyId"));
        assert!(code.contains("impl TryFrom < String > for MyId"));
        assert!(code.contains("impl TryFrom < & str > for MyId"));
    }
}
