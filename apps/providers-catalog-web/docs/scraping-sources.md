# Harness plan and usage sources

The Rust ingestion binary (`apps/providers-catalog-ingest`) fetches one official
page per harness. A page that stops matching its expected plan or limit text is
reported as a scrape failure rather than silently producing no rows. Every run
re-reads the live pages, so a plan or allowance change on the source replaces
the previous facts for that harness.

| Harness | Source | What it provides | Caveats |
| --- | --- | --- | --- |
| Claude Code | <https://claude.com/pricing> | Pro, Max 5x and Max 20x prices; Claude Code is included from Pro upward | Usage is session- and week-based, not a request count, and there is no free tier. Prices are the web subscription prices. |
| Codex | <https://developers.openai.com/codex/pricing> | Free, Go, Plus and Pro inclusion; API-key option | Usage is a shared ChatGPT allowance; reported as plan text, not a fixed request count. |
| Gemini CLI | <https://raw.githubusercontent.com/google-gemini/gemini-cli/main/docs/resources/quota-and-pricing.md> | Free tier: 1,000 model requests per user per day with a Google account, 250 per day with an unpaid API key | The repository README says the personal-Google route moved to Antigravity CLI in June 2026; the quota document is the authoritative table and is versioned with the CLI. |
| OpenCode | <https://opencode.ai/pricing> | Go at $10/month, Go Plus at $40/month, Go Teams at $20/member/month; Zen is pay-as-you-go; free models have a daily request limit | The monthly limits are dollar allowances per model, not a fixed request count. The free daily limit is published without a number. |
| GitHub Copilot | <https://docs.github.com/en/copilot/get-started/plans> | Free, Pro, Pro+, Max, Business and Enterprise prices; Free includes 2,000 completions per month | Copilot bills chat and agent usage in AI credits on every plan, so the completions figure is the only fixed recurring allowance. |
| Cursor | <https://cursor.com/pricing> | Free, Plus, Premium, Super, Ultra and Teams prices | The free tier is described only as "limited", with no numeric allowance, so it stays a free plan without a `free_offers` row. Teams is priced per seat from $30. |
| Devin | <https://devin.ai/pricing> | Free, Pro, Max and Teams prices | `windsurf.com/pricing` redirects to this page, so Windsurf plans are read here. The free tier is a "light quota", not a number. |
| Kiro | <https://kiro.dev/pricing> | Free (50 credits per month), Pro, Pro+, Pro Max and Power prices | The 500-credit trial for first-time Pro upgrades is a trial and is excluded from `free_offers` by the offer policy. |
| OpenHands | <https://www.openhands.dev/pricing> | Agent Canvas (free, self-hosted), Teams and Enterprise | Teams is priced per ten users; the sessions cap is a plan limit in the page text, not a recurring free allowance. |
| Amazon Q Developer | <https://aws.amazon.com/q/developer/pricing/> | Free tier (50 agentic requests per month) and Pro at $19/user/month | Pro request limits are described as increased, with no number. |
| Warp | <https://www.warp.dev/pricing> | Free, Build, Max and Business prices | The pricing cards toggle monthly and annual billing; the scraped price is the monthly rate the page shows by default. The free tier allowance is "limited", not numeric. |
| Zed | <https://zed.dev/pricing> | Personal (free), Pro and Business prices | Pro includes $5 of hosted-model usage, a dollar credit rather than a request count; Business bundles no credits. |
| Augment Code | <https://www.augmentcode.com/pricing> | Standard and Business prices | Prices are per team up to 50 seats with dollar usage included, not a numeric request allowance. |
| Factory | <https://factory.ai/pricing> | Pro, Plus, Max and Teams prices | Rate limits are published as multipliers of Pro, not as request counts. |
| Replit Agent | <https://replit.com/pricing> | Core and Pro prices | Monthly list prices; usage is dollar-denominated and no free-tier allowance is published. |

Harnesses evaluated and left out because their plan pages cannot be scraped for
real prices: JetBrains AI renders its prices client-side in local currency,
Tabnine injects its per-user price text client-side, Continue's pricing URL now
returns 404, Sourcegraph publishes only a minimum annual enterprise contract,
Qodo prices reviews per credit without a plan price, Cerebras does not render
its tier prices in the page text, and Windsurf's pricing URL redirects to Devin,
which the Devin source already covers.

Only explicit numeric free allowances become `free_offers` rows. Plan prices
become `plans` rows. Usage that is expressed as a shared or dollar-denominated
allowance is kept as a subscription plan without inventing a request count.

The binary fetches the models.dev catalog in the same run to resolve provider
ids and names, then upserts the models.dev provider/model facts and the scraped
plan facts into the given DuckDB snapshot. Each scraped page gets a `sources`
row so plan facts keep their own URL, fetch time and content hash.

A harness that sells its own plans (Cursor, Devin, Kiro, OpenHands, Amazon Q
Developer, Warp, Zed, Augment Code, Factory, Replit) has no
models.dev provider entry. The ingest binary inserts that vendor as a provider
entry and links it to the harness, so the plan rows satisfy the provider join
and appear on the harness page without pretending the vendor serves an API
model. Vendors already present in models.dev keep their fetched provider facts.
