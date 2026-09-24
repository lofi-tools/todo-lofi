//! Model families, and which family/version a wire model id belongs to.
//!
//! A *family* is what stays the same no matter which gateway serves a model and
//! no matter which release it is: `glm`, `claude`, `qwen`. A *version* is one
//! release inside it (`5.3-flash`, `sonnet-5`). This module owns family
//! identity and the inference from a wire id to a [`ModelVersion`].
//!
//! It deliberately does **not** own version data — context windows, prices,
//! release dates and benchmark scores are observed at runtime and live in the
//! catalog store (see [`crate::catalog_store`]). That split is the whole point:
//! family identity is *logic* and belongs in code review, while the set of
//! versions a family currently has is *data* that changes weekly.
//!
//! The inference is best-effort by construction: gateway id conventions are
//! not standardized, so the rules below are documented and tested rather than
//! clever. A version this module gets wrong is a display and grouping problem,
//! never a correctness one — nothing downstream decides whether a request is
//! valid from it.
//!
//! The names used here are the same strings the quirks roster
//! ([`crate::quirks`]) uses as `model_families` config values, so "family name"
//! means one thing across the library.

/// A model line, recognized across every gateway that serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFamily {
    /// The family name, also usable as a `model_families` config value.
    pub name: &'static str,
    /// The organization the family comes from, for grouping in a UI.
    pub vendor: &'static str,
    /// Substrings that identify a model id as this family. Matched
    /// case-insensitively, so a gateway's casing never hides a family.
    pub markers: &'static [&'static str],
}

/// Every family this module recognizes, in match order.
///
/// Order is the one thing that matters here. A marker must be listed before any
/// broader marker it contains (`gpt-oss` before `gpt`) and before any marker
/// whose match would put the version in the wrong place: a
/// `llama-3.3-nemotron-super-49b-v1` id reads correctly as llama 3.3, whereas
/// matching `nemotron` first would report version 49.
pub fn families() -> &'static [ModelFamily] {
    &[
        ModelFamily {
            name: "compound",
            vendor: "groq",
            markers: &["compound"],
        },
        ModelFamily {
            name: "gpt-oss",
            vendor: "openai",
            markers: &["gpt-oss"],
        },
        ModelFamily {
            name: "laguna",
            vendor: "poolside",
            markers: &["laguna"],
        },
        ModelFamily {
            name: "devstral",
            vendor: "mistral",
            markers: &["devstral"],
        },
        ModelFamily {
            name: "mistral",
            vendor: "mistral",
            markers: &["mistral"],
        },
        ModelFamily {
            name: "llama",
            vendor: "meta",
            markers: &["llama"],
        },
        ModelFamily {
            name: "nemotron",
            vendor: "nvidia",
            markers: &["nemotron"],
        },
        ModelFamily {
            name: "claude",
            vendor: "anthropic",
            markers: &["claude"],
        },
        ModelFamily {
            name: "gemini",
            vendor: "google",
            markers: &["gemini"],
        },
        ModelFamily {
            name: "deepseek",
            vendor: "deepseek",
            markers: &["deepseek"],
        },
        ModelFamily {
            name: "qwen",
            vendor: "qwen",
            markers: &["qwen"],
        },
        ModelFamily {
            name: "kimi",
            vendor: "moonshot",
            markers: &["kimi"],
        },
        ModelFamily {
            name: "glm",
            vendor: "z-ai",
            markers: &["glm"],
        },
        ModelFamily {
            name: "grok",
            vendor: "x-ai",
            markers: &["grok"],
        },
        ModelFamily {
            name: "north",
            vendor: "cohere",
            markers: &["north"],
        },
        ModelFamily {
            name: "hy3",
            vendor: "tencent",
            markers: &["hy3", "hunyuan"],
        },
        ModelFamily {
            name: "gpt",
            vendor: "openai",
            markers: &["gpt"],
        },
    ]
}

/// The family `model` belongs to, if any. The provider prefix is ignored, so a
/// model resolves to the same family whichever gateway serves it.
pub fn family_for_model_id(model: &str) -> Option<&'static ModelFamily> {
    let lower = model.to_ascii_lowercase();
    families()
        .iter()
        .find(|family| anchor_in(&lower, family).is_some())
}

/// Where a family's marker sits in an already-lowercased id, and which marker
/// it was.
///
/// Two rules make this reliable, and both are needed:
///
/// * **A marker must start at a token boundary.** Otherwise the hosted-`ollama`
///   prefix would match the `llama` family, and every Ollama model would be
///   reported as a Meta one.
/// * **The last such occurrence wins**, because a model id repeats its vendor
///   (`deepseek/deepseek-v4-pro`). Anchoring on the first would put the version
///   read in the wrong place and leave `/deepseek-v` in the variant.
fn anchor_in(lower: &str, family: &ModelFamily) -> Option<(usize, &'static str)> {
    family
        .markers
        .iter()
        .filter_map(|marker| marker_position(lower, marker).map(|index| (index, *marker)))
        .max_by_key(|(index, _)| *index)
}

/// The last token-boundary-anchored occurrence of `marker` in `lower`.
fn marker_position(lower: &str, marker: &str) -> Option<usize> {
    let mut found = None;
    let mut search_from = 0;
    while let Some(offset) = lower.get(search_from..)?.find(marker) {
        let index = search_from + offset;
        let at_boundary = match lower[..index].chars().next_back() {
            None => true,
            Some(previous) => !previous.is_alphanumeric(),
        };
        if at_boundary {
            found = Some(index);
        }
        search_from = index + marker.len();
    }
    found
}

/// The family registered under `name`, if any.
pub fn family_by_name(name: &str) -> Option<&'static ModelFamily> {
    families().iter().find(|family| family.name == name)
}

/// One release inside a family, as inferred from a wire model id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVersion {
    pub family: &'static str,
    pub vendor: &'static str,
    /// The release number (`5.3`, `3.3`, `2512`). `None` for a family that
    /// carries no number, like `compound` or `north`.
    pub version: Option<String>,
    /// The distinguishing part of the id besides the number (`flash`,
    /// `pro-0813`, `70b-instruct`, `sonnet` for an id that puts it first).
    pub variant: Option<String>,
    /// A serving tier that marks a *different wire model* rather than a
    /// different model: `free`, `latest`.
    pub tier: Option<String>,
}

impl ModelVersion {
    /// A stable grouping key for this version: `family` plus the version
    /// number, ignoring variant and tier. Two models with the same key are
    /// releases of the same thing, which is what benchmark data can be joined
    /// on when the exact variant was never measured.
    pub fn group_key(&self) -> String {
        match &self.version {
            Some(version) => format!("{} {}", self.family, version),
            None => self.family.to_string(),
        }
    }

    /// The human-readable form, e.g. `qwen 3.8-max`.
    pub fn label(&self) -> String {
        let mut label = self.family.to_string();
        if let Some(version) = &self.version {
            label.push(' ');
            label.push_str(version);
        }
        if let Some(variant) = &self.variant {
            label.push('-');
            label.push_str(variant);
        }
        label
    }
}

/// Serving tiers, stripped before version inference so `…-free` does not read
/// as part of a variant name.
const TIERS: &[&str] = &["free", "latest", "nitro", "thinking"];

/// Letters that act as a version *sigil* rather than part of a name, as in
/// `kimi-k2.7` or `deepseek-v4`.
const VERSION_SIGILS: &[char] = &['v', 'k'];

/// Infer the family and version a wire model id belongs to.
///
/// Returns `None` when no family matches — a router pseudo-model like
/// `openrouter/auto` or `orcarouter/fusion` is not a model, and a gateway's
/// unlisted internal id should not be forced into a family it may not have.
pub fn infer_model_version(model: &str) -> Option<ModelVersion> {
    let lower = model.to_ascii_lowercase();
    let family = family_for_model_id(&lower)?;

    // The anchored marker is the split point: the version and variant live
    // after it, and anything before it (the vendor prefix) is behind us.
    let (marker_start, marker) = anchor_in(&lower, family)?;
    let marker_end = marker_start + marker.len();

    let (remainder, tier) = strip_tier(lower.get(marker_end..)?);
    let remainder = remainder.trim_matches(is_separator);

    let (version, before, after) = split_version(remainder)
        // A family whose name ends in its number (`hy3`) has nothing left to
        // read, so the number comes from the marker itself.
        .or_else(|| digits_in(marker).map(|digits| (digits, String::new(), String::new())))
        .unwrap_or_else(|| (String::new(), remainder.to_string(), String::new()));

    let version = if version.is_empty() {
        None
    } else {
        Some(version)
    };
    let variant = join_parts(before, after);

    Some(ModelVersion {
        family: family.name,
        vendor: family.vendor,
        version,
        variant,
        tier,
    })
}

/// Characters that separate the parts of a model id rather than belonging to
/// any of them.
fn is_separator(character: char) -> bool {
    matches!(character, '-' | '_' | '.' | ' ' | '/')
}

/// Strip a trailing serving tier (`:free`, `-free`, `:latest`) from the
/// remainder, returning it and the tier's name.
fn strip_tier(remainder: &str) -> (&str, Option<String>) {
    let trimmed = remainder.trim_end_matches(['_', '.', ' ']);
    for separator in [':', '-'] {
        if let Some((head, tail)) = trimmed.rsplit_once(separator)
            && TIERS.contains(&tail)
        {
            return (head, Some(tail.to_string()));
        }
    }
    (remainder, None)
}

/// Split `remainder` into (version, before, after): the first numeric run, plus
/// whatever sits on either side of it.
///
/// A leading version sigil (`v` in `deepseek-v4`, `k` in `kimi-k2.7`) is
/// consumed with the number rather than left in the variant.
fn split_version(remainder: &str) -> Option<(String, String, String)> {
    let characters: Vec<char> = remainder.chars().collect();
    let mut index = 0;

    // A sigil only counts when a digit follows it immediately, and it is part
    // of the version rather than the variant (`deepseek-v4` is version 4).
    let mut sigil = false;
    if let Some(first) = characters.first()
        && VERSION_SIGILS.contains(first)
        && characters.get(1).is_some_and(char::is_ascii_digit)
    {
        index = 1;
        sigil = true;
    }

    if !characters.get(index).is_some_and(char::is_ascii_digit) {
        // The number is not at the front (`claude-sonnet-5`), so look for the
        // first digit anywhere and treat what precedes it as a variant part.
        index = characters.iter().position(char::is_ascii_digit)?;
        sigil = false;
    }

    let start = index;
    // Everything before the sigil is the variant; the sigil itself belongs to
    // the version.
    let before_end = if sigil {
        start.saturating_sub(1)
    } else {
        start
    };
    while characters
        .get(index)
        .is_some_and(|character| character.is_ascii_digit() || *character == '.')
    {
        index += 1;
    }

    // `5.` and `5.3.` are the same version as `5` and `5.3`.
    let version: String = characters[start..index]
        .iter()
        .collect::<String>()
        .trim_end_matches('.')
        .to_string();
    if version.is_empty() {
        return None;
    }

    let before: String = characters[..before_end].iter().collect();
    let after: String = characters[index..].iter().collect();
    Some((version, before, after))
}

/// The first run of digits in `text`, used when a family's number is inside its
/// own name (`hy3`).
fn digits_in(text: &str) -> Option<String> {
    let characters: Vec<char> = text.chars().collect();
    let start = characters.iter().position(char::is_ascii_digit)?;
    let mut end = start;
    while characters.get(end).is_some_and(char::is_ascii_digit) {
        end += 1;
    }
    Some(characters[start..end].iter().collect())
}

/// Join the parts either side of the version into a variant name, dropping the
/// empty ones and any leftover separators.
fn join_parts(before: String, after: String) -> Option<String> {
    let clean = |part: String| part.trim_matches(is_separator).to_string();
    let before = clean(before);
    let after = clean(after);
    let joined = match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (false, true) => before,
        (true, false) => after,
        (false, false) => format!("{before}-{after}"),
    };
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(model: &str) -> ModelVersion {
        infer_model_version(model).unwrap_or_else(|| panic!("{model} must resolve"))
    }

    #[test]
    fn reads_a_version_and_variant_from_a_numbered_id() {
        let glm = version("z-ai/glm-5.3-flash");
        assert_eq!(glm.family, "glm");
        assert_eq!(glm.version.as_deref(), Some("5.3"));
        assert_eq!(glm.variant.as_deref(), Some("flash"));
        assert_eq!(glm.tier, None);

        let gemini = version("google/gemini-3.7-flash");
        assert_eq!(gemini.family, "gemini");
        assert_eq!(gemini.version.as_deref(), Some("3.7"));
        assert_eq!(gemini.variant.as_deref(), Some("flash"));
    }

    #[test]
    fn a_vendor_prefix_does_not_change_the_family() {
        for id in [
            "google/gemini-3.8-flash",
            "opencode-zen/gemini-3.8-flash",
            "orcarouter/oc-google/gemini-3.8-flash",
        ] {
            assert_eq!(version(id).family, "gemini", "{id}");
        }
    }

    #[test]
    fn consumes_a_version_sigil_instead_of_leaving_it_in_the_variant() {
        let deepseek = version("deepseek/deepseek-v4-pro-0813");
        assert_eq!(deepseek.family, "deepseek");
        assert_eq!(deepseek.version.as_deref(), Some("4"));
        assert_eq!(deepseek.variant.as_deref(), Some("pro-0813"));

        let kimi = version("moonshotai/kimi-k2.7-code");
        assert_eq!(kimi.family, "kimi");
        assert_eq!(kimi.version.as_deref(), Some("2.7"));
        assert_eq!(kimi.variant.as_deref(), Some("code"));
    }

    #[test]
    fn reads_a_version_that_follows_the_variant_name() {
        let claude = version("anthropic/claude-sonnet-5");
        assert_eq!(claude.family, "claude");
        assert_eq!(claude.version.as_deref(), Some("5"));
        assert_eq!(claude.variant.as_deref(), Some("sonnet"));

        let qwen = version("qwen/qwen3-coder-next");
        assert_eq!(qwen.family, "qwen");
        assert_eq!(qwen.version.as_deref(), Some("3"));
        assert_eq!(qwen.variant.as_deref(), Some("coder-next"));
    }

    #[test]
    fn a_named_family_with_no_number_still_resolves() {
        let compound = version("groq/compound-mini");
        assert_eq!(compound.family, "compound");
        assert_eq!(compound.version, None);
        assert_eq!(compound.variant.as_deref(), Some("mini"));

        let north = version("cohere/north-mini-code");
        assert_eq!(north.family, "north");
        assert_eq!(north.version, None);
        assert_eq!(north.variant.as_deref(), Some("mini-code"));
    }

    #[test]
    fn a_number_inside_the_family_name_is_the_version() {
        let hy3 = version("tencent/hy3-free");
        assert_eq!(hy3.family, "hy3");
        assert_eq!(hy3.version.as_deref(), Some("3"));
        assert_eq!(hy3.tier.as_deref(), Some("free"));
        // The hynyuan alias lands on the same family.
        assert_eq!(version("b.ai/hunyuan-3-pro").family, "hy3");
    }

    #[test]
    fn a_serving_tier_is_reported_separately_from_the_variant() {
        let free = version("z-ai/glm-5.3-flash-free");
        assert_eq!(free.version.as_deref(), Some("5.3"));
        assert_eq!(free.variant.as_deref(), Some("flash"));
        assert_eq!(free.tier.as_deref(), Some("free"));

        let tagged = version("openai/gpt-oss-20b:free");
        assert_eq!(tagged.family, "gpt-oss");
        assert_eq!(tagged.tier.as_deref(), Some("free"));

        let latest = version("ollama/qwen3.8:latest");
        assert_eq!(latest.family, "qwen");
        assert_eq!(latest.tier.as_deref(), Some("latest"));
    }

    #[test]
    fn a_marker_inside_a_vendor_name_is_not_a_match() {
        // `ollama` contains `llama`, so without boundary anchoring every Ollama
        // model would be reported as a Meta family.
        assert_eq!(version("ollama/qwen3.8:latest").family, "qwen");
        // A genuine llama served by Ollama still resolves to llama.
        assert_eq!(version("ollama/llama-3.3-70b").family, "llama");
        // The same trap on the identity lookup.
        assert_eq!(
            family_for_model_id("ollama/qwen3.8").map(|f| f.name),
            Some("qwen")
        );
        assert_eq!(
            family_for_model_id("ollama/gpt-oss:120b").map(|f| f.name),
            Some("gpt-oss")
        );
    }

    #[test]
    fn a_more_specific_marker_wins_over_a_broader_one() {
        // `gpt-oss` must beat `gpt`, or every gpt-oss release would look like a
        // GPT release.
        assert_eq!(version("openai/gpt-oss-120b").family, "gpt-oss");
        assert_eq!(version("openai/gpt-5.4").family, "gpt");
        // A llama id that names nemotron reads as llama, keeping its version
        // where the number belongs.
        let llama = version("nvidia/llama-3.3-nemotron-super-49b-v1");
        assert_eq!(llama.family, "llama");
        assert_eq!(llama.version.as_deref(), Some("3.3"));
    }

    #[test]
    fn a_router_pseudo_model_is_not_a_family_member() {
        for id in ["openrouter/auto", "openrouter/free", "orcarouter/fusion"] {
            assert!(infer_model_version(id).is_none(), "{id}");
        }
    }

    #[test]
    fn the_group_key_ignores_variant_and_tier() {
        let pro = version("deepseek/deepseek-v4-pro-0813");
        let flash = version("deepseek/deepseek-v4-flash");
        assert_eq!(pro.group_key(), flash.group_key());
        assert_eq!(pro.group_key(), "deepseek 4");
        // A family with no number keys on its name alone.
        assert_eq!(version("groq/compound").group_key(), "compound");
    }

    #[test]
    fn labels_read_like_a_model_name() {
        assert_eq!(version("z-ai/glm-5.3-flash").label(), "glm 5.3-flash");
        assert_eq!(
            version("qwen/qwen3-coder-next").label(),
            "qwen 3-coder-next"
        );
        assert_eq!(version("groq/compound-mini").label(), "compound-mini");
    }

    #[test]
    fn family_names_are_stable_and_lookup_by_name_works() {
        assert_eq!(family_by_name("glm").map(|f| f.vendor), Some("z-ai"));
        assert!(family_by_name("nonexistent").is_none());
        // Names are unique: a duplicate would make `model_families` config
        // values ambiguous.
        let mut names: Vec<&str> = families().iter().map(|f| f.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "family names must be unique");
    }

    #[test]
    fn the_quirks_roster_and_the_identity_roster_agree_on_hy3() {
        // The quirks roster is quirk-only and small; the identity roster is
        // broader. Where they overlap, the family name must be the same string.
        let quirks =
            crate::quirks::family_for_model("hy3").expect("hy3 is a built-in quirk family");
        assert_eq!(quirks.name, family_by_name(quirks.name).unwrap().name);
    }
}
