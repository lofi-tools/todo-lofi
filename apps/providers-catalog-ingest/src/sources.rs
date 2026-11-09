use anyhow::{Result, bail};
use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ScrapedPlan {
    pub plan_id: String,
    pub provider_id: String,
    pub name: String,
    pub usage_model: String,
    pub billing_period: Option<String>,
    pub price: Option<f64>,
    pub currency: Option<String>,
    pub included_allowance: Option<f64>,
    pub allowance_unit: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScrapedOffer {
    pub offer_id: String,
    pub provider_id: String,
    pub plan_id: Option<String>,
    pub allowance: f64,
    pub allowance_unit: String,
    pub reset_period: String,
    pub conditions: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ParsedPage {
    pub plans: Vec<ScrapedPlan>,
    pub offers: Vec<ScrapedOffer>,
}

#[derive(Debug, Clone, Copy)]
pub enum Parser {
    Claude,
    Codex,
    Copilot,
    Cursor,
    Devin,
    Gemini,
    Kiro,
    OpenCode,
    OpenHands,
    AmazonQ,
    Augment,
    Factory,
    Replit,
    Warp,
    Zed,
}

pub struct HarnessSource {
    pub harness_id: &'static str,
    pub harness_name: &'static str,
    pub vendor: &'static str,
    pub homepage_url: &'static str,
    pub source_id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub parser: Parser,
    /// A vendor entry that exists in the catalog only because the harness sells
    /// its own plans, so models.dev does not carry it.
    pub vendor_provider: Option<(&'static str, &'static str)>,
}

pub const SOURCES: &[HarnessSource] = &[
    HarnessSource {
        harness_id: "claude-code",
        harness_name: "Claude Code",
        vendor: "Anthropic",
        homepage_url: "https://www.anthropic.com/claude-code",
        source_id: "claude-pricing",
        name: "Claude pricing",
        url: "https://claude.com/pricing",
        parser: Parser::Claude,
        vendor_provider: None,
    },
    HarnessSource {
        harness_id: "codex",
        harness_name: "Codex",
        vendor: "OpenAI",
        homepage_url: "https://openai.com/codex",
        source_id: "codex-pricing",
        name: "Codex pricing",
        url: "https://developers.openai.com/codex/pricing",
        parser: Parser::Codex,
        vendor_provider: None,
    },
    HarnessSource {
        harness_id: "gemini-cli",
        harness_name: "Gemini CLI",
        vendor: "Google",
        homepage_url: "https://github.com/google-gemini/gemini-cli",
        source_id: "gemini-cli-quota",
        name: "Gemini CLI quotas and pricing",
        url: "https://raw.githubusercontent.com/google-gemini/gemini-cli/main/docs/resources/quota-and-pricing.md",
        parser: Parser::Gemini,
        vendor_provider: None,
    },
    HarnessSource {
        harness_id: "opencode",
        harness_name: "OpenCode",
        vendor: "OpenCode",
        homepage_url: "https://opencode.ai",
        source_id: "opencode-pricing",
        name: "OpenCode Go plans",
        url: "https://opencode.ai/docs/go/",
        parser: Parser::OpenCode,
        vendor_provider: None,
    },
    HarnessSource {
        harness_id: "copilot",
        harness_name: "GitHub Copilot",
        vendor: "GitHub",
        homepage_url: "https://github.com/features/copilot",
        source_id: "copilot-plans",
        name: "GitHub Copilot plans",
        url: "https://docs.github.com/en/copilot/get-started/plans",
        parser: Parser::Copilot,
        vendor_provider: None,
    },
    HarnessSource {
        harness_id: "cursor",
        harness_name: "Cursor",
        vendor: "Anysphere",
        homepage_url: "https://cursor.com",
        source_id: "cursor-pricing",
        name: "Cursor pricing",
        url: "https://cursor.com/pricing",
        parser: Parser::Cursor,
        vendor_provider: Some(("cursor", "Cursor")),
    },
    HarnessSource {
        harness_id: "devin",
        harness_name: "Devin",
        vendor: "Cognition",
        homepage_url: "https://devin.ai",
        source_id: "devin-pricing",
        name: "Devin plans and pricing",
        url: "https://devin.ai/pricing",
        parser: Parser::Devin,
        vendor_provider: Some(("devin", "Devin")),
    },
    HarnessSource {
        harness_id: "kiro",
        harness_name: "Kiro",
        vendor: "AWS",
        homepage_url: "https://kiro.dev",
        source_id: "kiro-pricing",
        name: "Kiro pricing",
        url: "https://kiro.dev/pricing",
        parser: Parser::Kiro,
        vendor_provider: Some(("kiro", "Kiro")),
    },
    HarnessSource {
        harness_id: "openhands",
        harness_name: "OpenHands",
        vendor: "All Hands AI",
        homepage_url: "https://www.openhands.dev",
        source_id: "openhands-pricing",
        name: "OpenHands pricing",
        url: "https://www.openhands.dev/pricing",
        parser: Parser::OpenHands,
        vendor_provider: Some(("openhands", "OpenHands")),
    },
    HarnessSource {
        harness_id: "amazon-q",
        harness_name: "Amazon Q Developer",
        vendor: "Amazon",
        homepage_url: "https://aws.amazon.com/q/developer/",
        source_id: "amazon-q-pricing",
        name: "Amazon Q Developer pricing",
        url: "https://aws.amazon.com/q/developer/pricing/",
        parser: Parser::AmazonQ,
        vendor_provider: Some(("amazon-q", "Amazon Q Developer")),
    },
    HarnessSource {
        harness_id: "warp",
        harness_name: "Warp",
        vendor: "Warp",
        homepage_url: "https://www.warp.dev",
        source_id: "warp-pricing",
        name: "Warp pricing",
        url: "https://www.warp.dev/pricing",
        parser: Parser::Warp,
        vendor_provider: Some(("warp", "Warp")),
    },
    HarnessSource {
        harness_id: "zed",
        harness_name: "Zed",
        vendor: "Zed Industries",
        homepage_url: "https://zed.dev",
        source_id: "zed-pricing",
        name: "Zed pricing",
        url: "https://zed.dev/pricing",
        parser: Parser::Zed,
        vendor_provider: Some(("zed", "Zed")),
    },
    HarnessSource {
        harness_id: "augment-code",
        harness_name: "Augment Code",
        vendor: "Augment Code",
        homepage_url: "https://www.augmentcode.com",
        source_id: "augment-code-pricing",
        name: "Augment Code pricing",
        url: "https://www.augmentcode.com/pricing",
        parser: Parser::Augment,
        vendor_provider: Some(("augment-code", "Augment Code")),
    },
    HarnessSource {
        harness_id: "factory",
        harness_name: "Factory",
        vendor: "Factory",
        homepage_url: "https://factory.ai",
        source_id: "factory-pricing",
        name: "Factory pricing",
        url: "https://factory.ai/pricing",
        parser: Parser::Factory,
        vendor_provider: Some(("factory", "Factory")),
    },
    HarnessSource {
        harness_id: "replit",
        harness_name: "Replit Agent",
        vendor: "Replit",
        homepage_url: "https://replit.com",
        source_id: "replit-pricing",
        name: "Replit pricing",
        url: "https://replit.com/pricing",
        parser: Parser::Replit,
        vendor_provider: Some(("replit", "Replit")),
    },
];

impl HarnessSource {
    pub fn parse(&self, html: &str) -> Result<ParsedPage> {
        let text = html_to_text(html)?;
        let parsed = match self.parser {
            Parser::Claude => parse_claude(&text)?,
            Parser::Codex => parse_codex(&text)?,
            Parser::Copilot => parse_copilot(&text)?,
            Parser::Cursor => parse_cursor(&text)?,
            Parser::Devin => parse_devin(&text)?,
            Parser::Gemini => parse_gemini(&text)?,
            Parser::Kiro => parse_kiro(&text)?,
            Parser::OpenCode => parse_opencode(&text)?,
            Parser::OpenHands => parse_openhands(&text)?,
            Parser::AmazonQ => parse_amazon_q(&text)?,
            Parser::Augment => parse_augment(&text)?,
            Parser::Factory => parse_factory(&text)?,
            Parser::Replit => parse_replit(&text)?,
            Parser::Warp => parse_warp(&text)?,
            Parser::Zed => parse_zed(&text)?,
        };
        if parsed.plans.is_empty() {
            bail!("{} did not match any plan text", self.url);
        }
        Ok(parsed)
    }
}

pub fn html_to_text(html: &str) -> Result<String> {
    let script = Regex::new(r"(?is)<script\b[^>]*>.*?</script>")?;
    let style = Regex::new(r"(?is)<style\b[^>]*>.*?</style>")?;
    let noscript = Regex::new(r"(?is)<noscript\b[^>]*>.*?</noscript>")?;
    let tags = Regex::new(r"(?s)<[^>]+>")?;
    let entities = Regex::new(r"&(?:amp|nbsp|quot|#39|lt|gt);")?;
    let whitespace = Regex::new(r"\s+")?;

    let without_script = script.replace_all(html, " ");
    let without_style = style.replace_all(&without_script, " ");
    let without_scripts = noscript.replace_all(&without_style, " ");
    let without_tags = tags.replace_all(&without_scripts, " ");
    let decoded =
        entities.replace_all(&without_tags, |caps: &regex::Captures<'_>| match &caps[0] {
            "&amp;" => "&",
            "&nbsp;" => " ",
            "&quot;" => "\"",
            "&#39;" => "'",
            "&lt;" => "<",
            "&gt;" => ">",
            _ => " ",
        });
    Ok(whitespace.replace_all(&decoded, " ").trim().to_string())
}

fn slug(value: &str) -> String {
    value
        .to_lowercase()
        .replace('+', "-plus")
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn parse_number(value: &str) -> Option<f64> {
    value.replace(',', "").trim().parse::<f64>().ok()
}

fn plan(
    provider_id: &str,
    name: &str,
    usage_model: &str,
    billing_period: Option<&str>,
    price: Option<f64>,
    allowance: Option<(f64, &str)>,
) -> ScrapedPlan {
    ScrapedPlan {
        plan_id: format!("{provider_id}-{}", slug(name)),
        provider_id: provider_id.to_string(),
        name: name.to_string(),
        usage_model: usage_model.to_string(),
        billing_period: billing_period.map(str::to_string),
        price,
        currency: price.map(|_| "USD".to_string()),
        included_allowance: allowance.map(|(amount, _)| amount),
        allowance_unit: allowance.map(|(_, unit)| unit.to_string()),
    }
}

fn offer(
    plan: &ScrapedPlan,
    allowance: f64,
    allowance_unit: &str,
    reset_period: &str,
    suffix: &str,
    conditions: Option<&str>,
) -> ScrapedOffer {
    ScrapedOffer {
        offer_id: format!("{}-{suffix}", plan.plan_id),
        provider_id: plan.provider_id.clone(),
        plan_id: Some(plan.plan_id.clone()),
        allowance,
        allowance_unit: allowance_unit.to_string(),
        reset_period: reset_period.to_string(),
        conditions: conditions.map(str::to_string),
    }
}

fn parse_claude(text: &str) -> Result<ParsedPage> {
    let cards = [
        ("free", "Claude Free"),
        ("pro", "Claude Pro"),
        ("max", "Claude Max"),
        ("ultra", "Claude Ultra"),
    ];
    // Pro advertises the annual price first and the monthly price in prose, so
    // the billed-monthly amount is the one a monthly plan row should carry.
    let pro_monthly = Regex::new(r"(?i)\bpro\b.{0,240}?\$\s*(\d+(?:\.\d+)?)\s*if billed monthly")?;
    let mut plans = Vec::new();
    for (suffix, name) in cards {
        let price = if suffix == "pro" {
            pro_monthly
                .captures(text)
                .and_then(|captures| parse_number(&captures[1]))
                .or_else(|| price_after_heading(text, "pro"))
        } else {
            price_after_heading(text, suffix)
        };
        let Some(price) = price else {
            continue;
        };
        let usage_model = if suffix == "free" {
            "free"
        } else {
            "subscription"
        };
        let mut entry = plan(
            "anthropic",
            name,
            usage_model,
            Some("month"),
            Some(price),
            None,
        );
        entry.plan_id = format!("anthropic-{suffix}");
        plans.push(entry);
    }
    Ok(ParsedPage {
        plans,
        offers: Vec::new(),
    })
}

fn price_after_heading(text: &str, heading: &str) -> Option<f64> {
    let pattern = format!(
        r"(?i)\b{}\b[^$]{{0,80}}\$\s*(\d+(?:\.\d+)?)",
        regex::escape(heading)
    );
    let regex = Regex::new(&pattern).ok()?;
    regex
        .captures(text)
        .and_then(|captures| parse_number(&captures[1]))
}

fn parse_codex(text: &str) -> Result<ParsedPage> {
    let re = Regex::new(r"(?i)\b(free|go|plus|pro)\b[^$]{0,60}\$\s*(\d+(?:\.\d+)?)")?;
    let mut plans = Vec::new();
    for captures in re.captures_iter(text) {
        let key = captures[1].to_lowercase();
        let Some(price) = parse_number(&captures[2]) else {
            continue;
        };
        let (name, usage_model, suffix) = match key.as_str() {
            "free" => ("ChatGPT Free", "free", "free"),
            "go" => ("ChatGPT Go", "subscription", "go"),
            "plus" => ("ChatGPT Plus", "subscription", "plus"),
            _ => ("ChatGPT Pro", "subscription", "pro"),
        };
        let plan_id = format!("openai-{suffix}");
        if plans
            .iter()
            .any(|entry: &ScrapedPlan| entry.plan_id == plan_id)
        {
            continue;
        }
        let mut entry = plan(
            "openai",
            name,
            usage_model,
            Some("month"),
            Some(price),
            None,
        );
        entry.plan_id = plan_id;
        plans.push(entry);
    }
    Ok(ParsedPage {
        plans,
        offers: Vec::new(),
    })
}

fn parse_gemini(text: &str) -> Result<ParsedPage> {
    // The quota table is the authoritative list. Only the unpaid rows are free
    // tiers; the paid Google AI and Workspace rows are subscription quotas.
    let row = Regex::new(r"(?i)\|\s*([^|]*?)\s*\|\s*([^|]*?)\s*\|\s*([\d,]+)\s+requests\s*\|")?;
    let mut plans: Vec<ScrapedPlan> = Vec::new();
    let mut offers = Vec::new();
    for captures in row.captures_iter(text) {
        let auth = captures[1].to_lowercase();
        let tier = captures[2].to_lowercase();
        let Some(allowance) = parse_number(&captures[3]) else {
            continue;
        };
        let (name, plan_id) = if tier.contains("free tier") {
            (
                "Gemini CLI free (API key)",
                "google-gemini-cli-free-api-key",
            )
        } else if auth.contains("google account") && tier.contains("individual") {
            (
                "Gemini CLI free (Google account)",
                "google-gemini-cli-free-google",
            )
        } else {
            continue;
        };
        if plans.iter().any(|entry| entry.plan_id == plan_id) {
            continue;
        }
        let mut entry = plan(
            "google",
            name,
            "free",
            None,
            Some(0.0),
            Some((allowance, "requests")),
        );
        entry.plan_id = plan_id.to_string();
        offers.push(offer(
            &entry,
            allowance,
            "requests",
            "day",
            "requests",
            Some("Free tier, per user per day."),
        ));
        plans.push(entry);
    }
    Ok(ParsedPage { plans, offers })
}

fn parse_opencode(text: &str) -> Result<ParsedPage> {
    // The marketing pricing page renders its cards in the browser, so the
    // versioned Go docs page is the scrape target. It sells the Go tiers only;
    // OpenCode Zen is pay-as-you-go and not priced there, so it stays a company
    // plan rather than a scraped subscription.
    let re = Regex::new(
        r"(?i)\b(go plus|go teams|go)\b\s*\$\s*(\d+(?:\.\d+)?)\s*(?:/|per)\s*(?:member\s*(?:/|per)\s*)?month",
    )?;
    let mut plans: Vec<ScrapedPlan> = Vec::new();
    for captures in re.captures_iter(text) {
        let Some(price) = parse_number(&captures[2]) else {
            continue;
        };
        let (name, suffix) = match captures[1].to_lowercase().as_str() {
            "go plus" => ("OpenCode Go Plus", "opencode-go-plus"),
            "go teams" => ("OpenCode Go Teams", "opencode-go-teams"),
            _ => ("OpenCode Go", "opencode-go"),
        };
        if plans.iter().any(|entry| entry.plan_id == suffix) {
            continue;
        }
        let mut entry = plan(
            "opencode-go",
            name,
            "subscription",
            Some("month"),
            Some(price),
            None,
        );
        entry.plan_id = suffix.to_string();
        plans.push(entry);
    }
    Ok(ParsedPage {
        plans,
        offers: Vec::new(),
    })
}

fn parse_copilot(text: &str) -> Result<ParsedPage> {
    let keyword = Regex::new(r"(?i)copilot\s+(free|pro\+|pro|max|business|enterprise)")?;
    let price_re = Regex::new(r"(?i)^[^$0-9]{0,80}\$\s*(\d+(?:\.\d+)?)\s*usd")?;
    let plan_name = Regex::new(r"(?i)copilot")?;
    let mut plans = Vec::new();
    for captures in keyword.captures_iter(text) {
        let key = captures[1].to_lowercase();
        // A table row for one plan can sit right before another plan's price, so
        // stop the window at the next plan name instead of taking its price.
        let end = captures
            .get(0)
            .map(|entry| entry.end())
            .unwrap_or(text.len());
        let rest = &text[end..(end + 120).min(text.len())];
        let cut = plan_name
            .find(rest)
            .map(|entry| entry.start())
            .unwrap_or(rest.len());
        let Some(price_captures) = price_re.captures(&rest[..cut]) else {
            continue;
        };
        let Some(price) = parse_number(&price_captures[1]) else {
            continue;
        };
        let (name, suffix) = match key.as_str() {
            "free" => ("GitHub Copilot Free", "copilot-free"),
            "pro+" => ("GitHub Copilot Pro+", "copilot-pro-plus"),
            "pro" => ("GitHub Copilot Pro", "copilot-pro"),
            "max" => ("GitHub Copilot Max", "copilot-max"),
            "business" => ("GitHub Copilot Business", "copilot-business"),
            _ => ("GitHub Copilot Enterprise", "copilot-enterprise"),
        };
        if plans
            .iter()
            .any(|entry: &ScrapedPlan| entry.plan_id == suffix)
        {
            continue;
        }
        let usage_model = if key == "free" {
            "free"
        } else {
            "subscription"
        };
        let mut entry = plan(
            "github-copilot",
            name,
            usage_model,
            Some("month"),
            Some(price),
            None,
        );
        entry.plan_id = suffix.to_string();
        plans.push(entry);
    }

    if !plans.iter().any(|entry| entry.plan_id == "copilot-free")
        && text.to_lowercase().contains("copilot free")
    {
        let mut free_plan = plan(
            "github-copilot",
            "GitHub Copilot Free",
            "free",
            Some("month"),
            Some(0.0),
            None,
        );
        free_plan.plan_id = "copilot-free".to_string();
        plans.push(free_plan);
    }

    let mut offers = Vec::new();
    if let Some(free_plan) = plans.iter().find(|entry| entry.plan_id == "copilot-free") {
        let completions = Regex::new(r"(?i)([\d,]+)\s+(?:code\s+)?completions")?;
        if let Some(captures) = completions.captures(text)
            && let Some(allowance) = parse_number(&captures[1])
        {
            offers.push(offer(
                free_plan,
                allowance,
                "completions",
                "month",
                "completions",
                Some("Copilot Free monthly code completions."),
            ));
        }
    }

    Ok(ParsedPage { plans, offers })
}

fn parse_cursor(text: &str) -> Result<ParsedPage> {
    let re =
        Regex::new(r"(?i)\b(free|plus|premium|super|ultra|teams)\b[^$]{0,40}\$\s*(\d+(?:\.\d+)?)")?;
    let mut plans: Vec<ScrapedPlan> = Vec::new();
    for captures in re.captures_iter(text) {
        let Some(price) = parse_number(&captures[2]) else {
            continue;
        };
        let key = captures[1].to_lowercase();
        let (name, plan_id) = match key.as_str() {
            "free" => ("Cursor Free", "cursor-free"),
            "plus" => ("Cursor Plus", "cursor-plus"),
            "premium" => ("Cursor Premium", "cursor-premium"),
            "super" => ("Cursor Super", "cursor-super"),
            "ultra" => ("Cursor Ultra", "cursor-ultra"),
            _ => ("Cursor Teams", "cursor-teams"),
        };
        if plans.iter().any(|entry| entry.plan_id == plan_id) {
            continue;
        }
        let usage_model = if key == "free" {
            "free"
        } else {
            "subscription"
        };
        let mut entry = plan(
            "cursor",
            name,
            usage_model,
            Some("month"),
            Some(price),
            None,
        );
        entry.plan_id = plan_id.to_string();
        plans.push(entry);
    }
    Ok(ParsedPage {
        plans,
        offers: Vec::new(),
    })
}

fn parse_devin(text: &str) -> Result<ParsedPage> {
    let re = Regex::new(r"(?i)\b(free|pro|max|teams)\b[^$]{0,50}\$\s*(\d+(?:\.\d+)?)")?;
    let mut plans: Vec<ScrapedPlan> = Vec::new();
    for captures in re.captures_iter(text) {
        let Some(price) = parse_number(&captures[2]) else {
            continue;
        };
        let key = captures[1].to_lowercase();
        let (name, plan_id) = match key.as_str() {
            "free" => ("Devin Free", "devin-free"),
            "pro" => ("Devin Pro", "devin-pro"),
            "max" => ("Devin Max", "devin-max"),
            _ => ("Devin Teams", "devin-teams"),
        };
        if plans.iter().any(|entry| entry.plan_id == plan_id) {
            continue;
        }
        let usage_model = if key == "free" {
            "free"
        } else {
            "subscription"
        };
        let mut entry = plan("devin", name, usage_model, Some("month"), Some(price), None);
        entry.plan_id = plan_id.to_string();
        plans.push(entry);
    }
    Ok(ParsedPage {
        plans,
        offers: Vec::new(),
    })
}

fn parse_kiro(text: &str) -> Result<ParsedPage> {
    let re =
        Regex::new(r"(?i)kiro\s+(free|pro\s*\+|pro max|pro|power)[^$]{0,60}\$\s*(\d+(?:\.\d+)?)")?;
    let mut plans: Vec<ScrapedPlan> = Vec::new();
    let mut offers = Vec::new();
    for captures in re.captures_iter(text) {
        let Some(price) = parse_number(&captures[2]) else {
            continue;
        };
        let key = captures[1]
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let (name, plan_id) = match key.as_str() {
            "free" => ("Kiro Free", "kiro-free"),
            "pro+" => ("Kiro Pro+", "kiro-pro-plus"),
            "pro max" => ("Kiro Pro Max", "kiro-pro-max"),
            "power" => ("Kiro Power", "kiro-power"),
            _ => ("Kiro Pro", "kiro-pro"),
        };
        if plans.iter().any(|entry| entry.plan_id == plan_id) {
            continue;
        }
        let usage_model = if key == "free" {
            "free"
        } else {
            "subscription"
        };
        let mut entry = plan("kiro", name, usage_model, Some("month"), Some(price), None);
        entry.plan_id = plan_id.to_string();
        plans.push(entry);
    }

    if let Some(free_plan) = plans.iter().find(|entry| entry.plan_id == "kiro-free") {
        // The free-plan card is the only place the per-month credit amount is a
        // recurring allowance; the trial credits elsewhere are excluded.
        let credits = Regex::new(r"(?is)kiro\s+free\b.{0,200}?([\d,]+)\s+credits")?;
        if let Some(captures) = credits.captures(text)
            && let Some(allowance) = parse_number(&captures[1])
        {
            offers.push(offer(
                free_plan,
                allowance,
                "credits",
                "month",
                "credits",
                Some("Kiro Free monthly credits."),
            ));
        }
    }

    Ok(ParsedPage { plans, offers })
}

fn parse_openhands(text: &str) -> Result<ParsedPage> {
    let mut plans: Vec<ScrapedPlan> = Vec::new();
    if text.to_lowercase().contains("agent canvas") {
        let mut entry = plan(
            "openhands",
            "OpenHands Agent Canvas",
            "free",
            None,
            Some(0.0),
            None,
        );
        entry.plan_id = "openhands-agent-canvas".to_string();
        plans.push(entry);
    }
    let teams = Regex::new(r"(?i)\bteams\b[^$]{0,20}\$\s*(\d+(?:\.\d+)?)")?;
    if let Some(captures) = teams.captures(text)
        && let Some(price) = parse_number(&captures[1])
    {
        let mut entry = plan(
            "openhands",
            "OpenHands Teams",
            "subscription",
            Some("month"),
            Some(price),
            None,
        );
        entry.plan_id = "openhands-teams".to_string();
        plans.push(entry);
    }
    Ok(ParsedPage {
        plans,
        offers: Vec::new(),
    })
}

/// Reads the price that follows a plan name. The match stops at a `$`, so a
/// plan card cannot reach into the next card's numbers.
fn keyword_plan_prices(
    text: &str,
    provider_id: &str,
    specs: &[(&str, &str, &str)],
    max_gap: usize,
    billing_period: Option<&str>,
) -> Result<Vec<ScrapedPlan>> {
    let keywords = specs
        .iter()
        .map(|(keyword, _, _)| regex::escape(keyword))
        .collect::<Vec<_>>()
        .join("|");
    let regex = Regex::new(&format!(
        r"(?i)\b({keywords})\b[^$]{{0,{max_gap}}}\$\s*(\d+(?:\.\d+)?)"
    ))?;
    let mut plans: Vec<ScrapedPlan> = Vec::new();
    for captures in regex.captures_iter(text) {
        let key = captures[1].to_lowercase();
        let Some((keyword, name, plan_id)) = specs
            .iter()
            .find(|(keyword, _, _)| keyword.eq_ignore_ascii_case(&key))
        else {
            continue;
        };
        if plans.iter().any(|entry| entry.plan_id == *plan_id) {
            continue;
        }
        let Some(price) = parse_number(&captures[2]) else {
            continue;
        };
        let usage_model = if *keyword == "free" || *keyword == "personal" {
            "free"
        } else {
            "subscription"
        };
        let mut entry = plan(
            provider_id,
            name,
            usage_model,
            billing_period,
            Some(price),
            None,
        );
        entry.plan_id = (*plan_id).to_string();
        plans.push(entry);
    }
    Ok(plans)
}

fn parse_warp(text: &str) -> Result<ParsedPage> {
    Ok(ParsedPage {
        plans: keyword_plan_prices(
            text,
            "warp",
            &[
                ("free", "Warp Free", "warp-free"),
                ("build", "Warp Build", "warp-build"),
                ("max", "Warp Max", "warp-max"),
                ("business", "Warp Business", "warp-business"),
            ],
            60,
            Some("month"),
        )?,
        offers: Vec::new(),
    })
}

fn parse_zed(text: &str) -> Result<ParsedPage> {
    Ok(ParsedPage {
        plans: keyword_plan_prices(
            text,
            "zed",
            &[
                ("personal", "Zed Personal", "zed-personal"),
                ("pro", "Zed Pro", "zed-pro"),
                ("business", "Zed Business", "zed-business"),
            ],
            40,
            Some("month"),
        )?,
        offers: Vec::new(),
    })
}

fn parse_augment(text: &str) -> Result<ParsedPage> {
    Ok(ParsedPage {
        plans: keyword_plan_prices(
            text,
            "augment-code",
            &[
                ("standard", "Augment Standard", "augment-standard"),
                ("business", "Augment Business", "augment-business"),
            ],
            30,
            Some("month"),
        )?,
        offers: Vec::new(),
    })
}

fn parse_factory(text: &str) -> Result<ParsedPage> {
    Ok(ParsedPage {
        plans: keyword_plan_prices(
            text,
            "factory",
            &[
                ("pro", "Factory Pro", "factory-pro"),
                ("plus", "Factory Plus", "factory-plus"),
                ("max", "Factory Max", "factory-max"),
                ("teams", "Factory Teams", "factory-teams"),
            ],
            30,
            Some("month"),
        )?,
        offers: Vec::new(),
    })
}

fn parse_replit(text: &str) -> Result<ParsedPage> {
    Ok(ParsedPage {
        plans: keyword_plan_prices(
            text,
            "replit",
            &[
                ("core", "Replit Core", "replit-core"),
                ("pro", "Replit Pro", "replit-pro"),
            ],
            20,
            Some("month"),
        )?,
        offers: Vec::new(),
    })
}

fn parse_amazon_q(text: &str) -> Result<ParsedPage> {
    let mut plans: Vec<ScrapedPlan> = Vec::new();
    let mut free = plan(
        "amazon-q",
        "Amazon Q Developer Free",
        "free",
        None,
        Some(0.0),
        None,
    );
    free.plan_id = "amazon-q-free".to_string();
    plans.push(free);
    let pro = Regex::new(r"(?i)pro tier[^$]{0,60}\$\s*(\d+(?:\.\d+)?)")?;
    if let Some(captures) = pro.captures(text)
        && let Some(price) = parse_number(&captures[1])
    {
        let mut entry = plan(
            "amazon-q",
            "Amazon Q Developer Pro",
            "subscription",
            Some("month"),
            Some(price),
            None,
        );
        entry.plan_id = "amazon-q-pro".to_string();
        plans.push(entry);
    }
    let mut offers = Vec::new();
    if let Some(free_plan) = plans.iter().find(|entry| entry.plan_id == "amazon-q-free") {
        let requests = Regex::new(r"(?i)([\d,]+)\s+agentic requests per month")?;
        if let Some(captures) = requests.captures(text)
            && let Some(allowance) = parse_number(&captures[1])
        {
            offers.push(offer(
                free_plan,
                allowance,
                "requests",
                "month",
                "agentic-requests",
                Some("Amazon Q Developer Free monthly agentic requests."),
            ));
        }
    }
    Ok(ParsedPage { plans, offers })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(harness_id: &str) -> &'static HarnessSource {
        SOURCES
            .iter()
            .find(|entry| entry.harness_id == harness_id)
            .expect("source exists")
    }

    #[test]
    fn parses_opencode_go_tiers() {
        let parsed = source("opencode")
            .parse("<p>Go $10/month</p><p>Go Plus $40/month</p><p>Go Teams $20/member/month</p>")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 3);
        let go = parsed
            .plans
            .iter()
            .find(|entry| entry.plan_id == "opencode-go")
            .expect("go plan");
        assert_eq!(go.price, Some(10.0));
        assert_eq!(go.provider_id, "opencode-go");
        assert!(
            parsed
                .plans
                .iter()
                .any(|entry| entry.plan_id == "opencode-go-plus")
        );
        assert!(
            parsed
                .plans
                .iter()
                .any(|entry| entry.plan_id == "opencode-go-teams")
        );
    }

    #[test]
    fn parses_claude_prices() {
        let parsed = source("claude-code")
            .parse("Free Try Claude $0 Free for everyone Pro For everyday work $17 Per month with annual subscription discount ( $200 billed up front). $20 if billed monthly. Max For all-day work $100 Per month Ultra For your longest tasks $200 Per month")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 4);
        assert_eq!(
            parsed
                .plans
                .iter()
                .find(|entry| entry.plan_id == "anthropic-pro")
                .expect("pro plan")
                .price,
            Some(20.0)
        );
        assert_eq!(
            parsed
                .plans
                .iter()
                .find(|entry| entry.plan_id == "anthropic-ultra")
                .expect("ultra plan")
                .price,
            Some(200.0)
        );
        assert!(parsed.offers.is_empty());
    }

    #[test]
    fn parses_copilot_free_allowances() {
        let parsed = source("copilot")
            .parse("Copilot Free includes an AI credit allowance. Copilot Pro $10 USD per month. Limited to 2000 completions per month on Copilot Free.")
            .expect("parses");
        assert!(
            parsed
                .plans
                .iter()
                .any(|entry| entry.plan_id == "copilot-free")
        );
        assert!(
            parsed
                .plans
                .iter()
                .any(|entry| entry.plan_id == "copilot-pro")
        );
        assert_eq!(parsed.offers.len(), 1);
        assert!(
            parsed
                .offers
                .iter()
                .any(|entry| entry.allowance_unit == "completions" && entry.allowance == 2000.0)
        );
    }

    #[test]
    fn parses_gemini_free_quotas() {
        let parsed = source("gemini-cli")
            .parse("| Authentication method | Tier / Subscription | Maximum requests per user per day | | **Google account** | Gemini Code Assist (Individual) | 1,000 requests | |  | Google AI Pro | 1,500 requests | | **Gemini API key** | Free tier (Unpaid) | 250 requests |")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 2);
        assert_eq!(parsed.offers.len(), 2);
        assert_eq!(parsed.offers[0].allowance, 1000.0);
        assert_eq!(parsed.offers[0].reset_period, "day");
        assert!(parsed.offers.iter().all(|entry| entry.plan_id.is_some()));
    }

    #[test]
    fn fails_loudly_when_a_page_does_not_match() {
        let error = source("codex")
            .parse("nothing useful here")
            .expect_err("must fail");
        assert!(error.to_string().contains("did not match"));
    }

    #[test]
    fn parses_cursor_tiers() {
        let parsed = source("cursor")
            .parse("Free $0 /month Plus $8 /month Premium $30 /month Super $100 /month Ultra $200 /month Teams $30 +/user/month")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 6);
        let ultra = parsed
            .plans
            .iter()
            .find(|entry| entry.plan_id == "cursor-ultra")
            .expect("ultra plan");
        assert_eq!(ultra.price, Some(200.0));
        assert_eq!(ultra.provider_id, "cursor");
        assert_eq!(
            parsed
                .plans
                .iter()
                .find(|entry| entry.plan_id == "cursor-free")
                .expect("free plan")
                .usage_model,
            "free"
        );
    }

    #[test]
    fn parses_devin_tiers() {
        let parsed = source("devin")
            .parse("Free $0 Download Pro POPULAR $20 per month Max NEW $200 per month Teams $80/month for team plan")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 4);
        assert_eq!(
            parsed
                .plans
                .iter()
                .find(|entry| entry.plan_id == "devin-teams")
                .expect("teams plan")
                .price,
            Some(80.0)
        );
    }

    #[test]
    fn parses_kiro_tiers_and_free_credits() {
        let parsed = source("kiro")
            .parse("KIRO FREE $0 per month 50 credits KIRO PRO $20 per user / month 1,000 credits KIRO PRO+ $40 per user / month KIRO PRO MAX $100 per user / month KIRO POWER $200 per user / month")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 5);
        assert!(
            parsed
                .plans
                .iter()
                .any(|entry| entry.plan_id == "kiro-pro-plus")
        );
        assert!(
            parsed
                .plans
                .iter()
                .any(|entry| entry.plan_id == "kiro-pro-max")
        );
        assert_eq!(parsed.offers.len(), 1);
        assert_eq!(parsed.offers[0].allowance, 50.0);
        assert_eq!(parsed.offers[0].allowance_unit, "credits");
    }

    #[test]
    fn parses_openhands_plans() {
        let parsed = source("openhands")
            .parse("Agent Canvas Free open source and free to self-host Teams $500/month Up to 10 users")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 2);
        assert_eq!(
            parsed
                .plans
                .iter()
                .find(|entry| entry.plan_id == "openhands-teams")
                .expect("teams plan")
                .price,
            Some(500.0)
        );
    }

    fn price_of(parsed: &ParsedPage, plan_id: &str) -> Option<f64> {
        parsed
            .plans
            .iter()
            .find(|entry| entry.plan_id == plan_id)
            .expect("plan exists")
            .price
    }

    #[test]
    fn parses_warp_tiers() {
        let parsed = source("warp")
            .parse("## Free $0 /month ## Build Recommended Monthly Annual Annual plan $18 /month ## Max Annual plan $180 /month ## Business $45 /user /month")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 4);
        assert_eq!(price_of(&parsed, "warp-build"), Some(18.0));
        assert_eq!(price_of(&parsed, "warp-business"), Some(45.0));
        assert!(parsed.offers.is_empty());
    }

    #[test]
    fn parses_zed_tiers() {
        let parsed = source("zed")
            .parse("## Personal $0 forever ## Pro Free Trial $10 per month ## Business $30 per seat, per month")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 3);
        assert_eq!(price_of(&parsed, "zed-pro"), Some(10.0));
        assert_eq!(price_of(&parsed, "zed-business"), Some(30.0));
        assert_eq!(
            parsed
                .plans
                .iter()
                .find(|entry| entry.plan_id == "zed-personal")
                .expect("personal plan")
                .usage_model,
            "free"
        );
    }

    #[test]
    fn parses_augment_tiers() {
        let parsed = source("augment-code")
            .parse("STANDARD $20 /month flat, no per-seat charge BUSINESS $100 /month flat, no per-seat charge ENTERPRISE Custom")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 2);
        assert_eq!(price_of(&parsed, "augment-standard"), Some(20.0));
        assert_eq!(price_of(&parsed, "augment-business"), Some(100.0));
    }

    #[test]
    fn parses_factory_tiers() {
        let parsed = source("factory")
            .parse("01 Pro Pro ## $20 /mo 02 Plus ## $100 /mo 03 Max ## $200 /mo 04 Teams ## $60/mo per team, plus $40/mo per seat")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 4);
        assert_eq!(price_of(&parsed, "factory-plus"), Some(100.0));
        assert_eq!(price_of(&parsed, "factory-teams"), Some(60.0));
    }

    #[test]
    fn parses_replit_tiers() {
        let parsed = source("replit")
            .parse("Core $20 $18 / month, billed annually Pro $100 $90 / month, billed annually Enterprise Custom")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 2);
        assert_eq!(price_of(&parsed, "replit-core"), Some(20.0));
        assert_eq!(price_of(&parsed, "replit-pro"), Some(100.0));
    }

    #[test]
    fn parses_amazon_q_free_and_pro() {
        let parsed = source("amazon-q")
            .parse("Free Tier: Advanced capabilities at zero cost Pro Tier: Expanded limits $19/mo. per user | Included agentic requests (Q&A chat, agentic coding) | 50 agentic requests per month | Included (with limits)")
            .expect("parses");
        assert_eq!(parsed.plans.len(), 2);
        assert_eq!(price_of(&parsed, "amazon-q-pro"), Some(19.0));
        assert_eq!(parsed.offers.len(), 1);
        assert_eq!(parsed.offers[0].allowance, 50.0);
        assert_eq!(parsed.offers[0].reset_period, "month");
    }
}
