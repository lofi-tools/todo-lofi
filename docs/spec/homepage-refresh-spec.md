# Homepage Refresh: user-facing features, roadmap, and a docs consistency pass — Spec

Status: Draft v1 — implemented. Every open question from the interview is
recorded below, and §11 records how the residual decisions were settled.

Purpose: Rebuild the landing page (`/`) around what the person using the app
cares about instead of what the code is made of. Drop the implementation- and
contributor-facing marketing, add the user-facing capabilities that are
currently buried in the docs, add a top-level `/roadmap` page (which absorbs the
feature inventory currently on `/docs/status`), and move the stack information
to the contributor docs.

---

## 1. Problem statement

The landing page currently spends its scarce attention on things a person
evaluating a task manager does not care about:

- **`LogoCloud` — "Built with: GPUI, Rust, SQLite, MCP, Astro, Panda CSS".**
  Pure implementation detail; also misleading, since two of those six are the
  website's own stack.
- **Hero subtitle** — "renders it with the same GPU-accelerated framework as
  Zed", "opt-in". Explains the build, not the product.
- **`StatsBand` — "Local first"** with a link labelled *"How the storage layer
  works"* pointing at `/docs/contributor/architecture`. A user-facing promise
  with a contributor-docs destination.
- **"Two guides, one for each job"** — a section that markets the
  documentation site's own information architecture, half of it aimed at people
  who want to modify the source.
- **Announcement pill — "Documentation for users and contributors".** An
  announcement about docs, above the product's own headline.
- **The automations block** — recipes, runs, phases, and "branches to clean
  up" as a standalone feature, presented with internal vocabulary.

At the same time, several things a user demonstrably *does* care about are
missing from the page entirely: personal and work tasks living in one list, the
priority/deadline scoring that decides what to do next, subtasks and blockers,
Todoist sync, the travel-checklist mini-app, and any statement of what is
coming next.

The page also leads with a promise the rest of the page does not develop: the
hero sells the local database, while four of the five feature blocks are about
the agent, workflows, and automations.

---

## 2. Positioning (normative)

The audience, in the words of the interview: **privacy-minded people leaving
SaaS task apps, developers who want an agent inside their task list, existing
Todoist/GitHub users, and general productivity users** — with this framing from
the interview being the sharpest differentiator:

> Most task lists are company-centric — you would not put "feed the dog" in
> GitHub or Linear. This one is centred on *you*, and holds your personal tasks
> and your company tasks in the same app.

The hero promise is: **an agent that does the work** (the only hero direction
chosen). The page then earns it by explaining what the agent does to *your
lists*, before widening to the rest of the product.

Tone: keep the house style — British spellings (`memorise`, `normalise`,
`centred`), em dashes, sentence case, no exclamation marks, no hype. Match the
docs' blunt, concrete voice.

---

## 3. Interview decisions (normative)

### 3.1 Round 1 — audience, cuts, length

| # | Question | Decision |
|---|----------|----------|
| 1 | Primary audience | Privacy-minded SaaS escapees; developers wanting an agent; existing Todoist/GitHub users; general productivity users. Overriding framing: **one app for personal *and* work tasks**. |
| 2 | Remove from the homepage | The "Built with" logo cloud; the "Two guides, one for each job" section; the "Documentation for users and contributors" pill; the technical hero subtitle. |
| 3 | Primary action | **Download the app** (`/downloads`), docs secondary. |
| 4 | Length | **Longer and richer** — more sections, each carrying more user-facing substance. |

### 3.2 Round 2 — features

| # | Question | Decision |
|---|----------|----------|
| 5 | Features to sell | Personal + work tasks in one list; offline/local/no account; the agent that edits your lists; priority that sorts itself; subtasks, blockers, dependencies; the repeat picker; the coding workflow with real diffs; Todoist two-way sync; the travel-checklist mini-app. Integrations grouped into their own section. A separate section introduces the "extensions" concept — *in-app workflows meant to replace simple single-purpose apps*. |
| 6 | Too rough to feature | Nothing was excluded outright. Rough/incomplete things stay, labelled. |
| 7 | Hero promise | **"An agent that does the work."** |
| 8 | Roadmap on the homepage | List future features marked **"coming soon"**, and add a **dedicated roadmap page**. |

Explicitly *not* selected (so: out of scope for the homepage): keyboard-first /
pickers, theming, the notification pane, model-provider configuration depth,
sync history / operation log.

### 3.3 Round 3 — structure

| # | Question | Decision |
|---|----------|----------|
| 9 | Notifications | **Dropped from the homepage.** `notifications.rs` is an app-wide error/warning log plus toasts, not a reminder system — selling it as "notifications/reminders" would be false. |
| 10 | Roadmap location | **`/roadmap`**, top-level, like `/downloads`. |
| 11 | Extensions scope | **Two sections**: one for the in-app workflows/extensions (mini-apps and workflow recipes), one for sync integrations (Todoist, GitHub). The coding workflow is folded into the extensions/workflows section as its worked example — it is the same "an app manages part of your lists" story. |
| 12 | Homepage sections | Hero; the roadmap teaser; the closing CTA; docs links; extensions/mini-apps; integrations; "one list for your whole life"; "offline and yours"; the agent in detail; the coding workflow inside the apps & workflows section; priority, subtasks, blockers, repeats. |
| 13 | Mock data | **Keep the current data** — except for one mandatory addition (below). |

### 3.4 Round 4 — order and scope

| # | Question | Decision |
|---|----------|----------|
| 14 | Narrative order | **Problem-led**: hero (agent) → one list for your whole life → offline/local-first → agent in detail → apps & workflows → priority & scheduling → integrations → roadmap teaser → docs → CTA. |
| 15 | Roadmap vs `/docs/status` | **The roadmap replaces the status page.** |
| 16 | Stack mentions | **Nowhere on the homepage.** Move the stack information to the contributor docs (see #22). |
| 17 | Contributor guide on the homepage | **Not featured** — footer only. |
| 18 | Test work | **Update the existing Playwright suite only** (no new test files). |
| 19 | Scope | A **whole-docs-site consistency pass**: homepage, the new roadmap page, the docs index, navigation, and the README. |

### 3.5 Round 5 — details

| # | Question | Decision |
|---|----------|----------|
| 20 | The "one list" visual | **Add two personal rows to the mock data** so the same list shows personal and work tasks side by side. |
| 21 | The old `/docs/status` URL | **Leave a one-line stub** that links to `/roadmap`. |
| 22 | Roadmap links | **Site header nav**, next to Docs and Download. |
| 23 | Closing CTA | **Download + Read the docs.** |
| 24 | Copy rules | Keep the house style (British spellings, em dashes, sentence case, no hype). |
| 25 | Off-limits claims | None requested. §10 records the factual constraints the code imposes anyway. |

### 3.6 Round 6 — roadmap content and stack destination

| # | Question | Decision |
|---|----------|----------|
| 26 | Roadmap structure | **Now / Next / Later** buckets. |
| 27 | Apps & workflows illustration | **The automations panel** (`AutomationsPanel.astro`). |
| 28 | Parked ideas | Add an **"Exploring"** bucket for vague ideas (and a short "Not planned" list; see §11.3). |
| 29 | Stack information | A **"Tech stack" section on `/docs/contributor/architecture`**. |

---

## 4. Cut list (normative)

Remove from `src/pages/index.astro`:

1. `<LogoCloud title="Built with" logos={[…]} />` and its import.
2. `<AnnouncementPill … />` and its import.
3. The "Two guides, one for each job" block (`guides` array, `linkStyle`,
   `ctaSecondary`, the whole bordered `<div>`) and the imports it needs
   (`Badge`, `SectionHeading` if unused elsewhere).
4. The technical hero subtitle's stack sentences.
5. The `StatsBand`'s contributor-docs link (`linkLabel="How the storage layer
   works"`, `linkHref="/docs/contributor/architecture"`) and its storage-layer
   copy.
6. The standalone `AutomationsPanel` block as *one of five equal feature
   blocks* — the component is kept, repurposed as the illustration for the
   apps & workflows section (§5.5).

Do **not** remove: the hero screenshot, the `StatsBand` component, the
`AutomationsPanel`, `IssueRow`, `Timeline`, or `ChatThread` illustrations.

Side effects to handle:

- `StatsBand` linked to `/docs/contributor/architecture`; check nothing else on
  the page depends on that anchor.
- The `guides` array is the only place the homepage links `/docs/contributor/*`
  and `/docs/configuration`. Removing it is *fine for the homepage* but the
  Playwright test "links into both guides, and every docs route is reachable
  from it" currently passes because `DocsFooter` lists every guide page. Verify
  before assuming; §9.2.
- `SectionHeading` and `Badge` are still needed by the new sections; keep the
  imports if used, drop them if not (the typecheck/`astro check` pass catches
  this).

---

## 5. New homepage outline (normative)

Order per §3.4 #14. Section headings below are the *intent*; final copy is
written during implementation, in the house voice.

### 5.1 Hero — "an agent that does the work"

- **Headline**: the agent promise, not the database. Current headline ("Your
  tasks, on your machine") becomes a candidate for the local-first section.
- **Subtitle**: what the agent does to your lists — adds and reshapes tasks,
  sets up repeats, drives a workflow, and shows the tools it calls. No
  framework names, no "opt-in", no "GPU-accelerated".
- **CTAs**: `Download` → `/downloads`, `Read the docs` → `/docs`.
- **Visual**: the existing `assets/app.png` screenshot and its alt text.
- **No `AnnouncementPill` slot.**

### 5.2 One list for your whole life

- The differentiator from §2: personal tasks and company tasks in one list, in
  an app that is yours rather than your employer's.
- **Visual**: the task-list illustration (`taskRows` → `IssueRow`), with **two
  personal rows added** (§3.5 #20). See §7.1 for the data change.
- **Link**: `/docs/tasks` ("Read about the task list"), or `/docs` if the
  section is framed as an introduction — decide once copy is drafted.
- Must not imply sharing or collaboration (§10).

### 5.3 Offline and yours (the local-first band)

- Keeps the `StatsBand` component and its role as a breath between two visual
  sections.
- **Copy**: the local database, no account, no telemetry, works with no
  network — the *user* consequence of local-first, not the storage design.
- **Link target must change**: no contributor-docs link. Candidate:
  "How sync stays optional" → `/docs/sync#offline`. §11.1 has the decision.

### 5.4 The agent, in detail

- Expands the hero claim: the pane is docked beside the list (not a separate
  window), it works in the same lists you see, you can watch the tools it calls
  and the files it touches.
- **Visual**: existing `ChatThread` + `agentThread` illustration.
- **Preview label** if the section mentions the coding side: the git-backed
  coding workflow is *preview* per `/docs/status`.

### 5.5 Apps & workflows (the extension surface)

The section the interview called the "extensions" concept: *in-app workflows
meant to replace simple single-purpose apps.*

- **Subsection — mini-apps**: travel checklists ship today; give it a trip and
  it generates the packing list and pre-departure chores as ordinary tasks.
  Message follow-ups, birthday wishes, and job-application tracking are
  **coming soon**.
- **Subsection — workflow recipes**: a task can carry a process with stages
  that have their own state; recipes are built from actions, timed waits, and
  events; the automations pane starts runs and hands you the decision.
- **Subsection — the coding workflow** (the worked example, folded in per §3.3
  #11): interview → spec → implement → review → merge, on a real checkout, so
  the diffs are real. Mark **preview**.
- **Visual**: `AutomationsPanel.astro` (per §3.6 #27) — it now reads as "an app
  that manages part of your lists" rather than as an internal recipe dump. The
  `KanbanBoard`/`codingColumns` illustration becomes unused on the homepage
  (§11.4).
- **Terminology**: define the words once, in this section, in user terms —
  *extensions* (built-in packs and workflow recipes) vs *integrations* (external
  sync, §5.6). This is the copy pass the whole-docs scope requires (§3.4 #19).

### 5.6 Integrations

- **Todoist** — two-way sync, available. Projects map onto projects, sections
  onto child tags; the local database stays the copy the app trusts.
- **GitHub** — issues and projects, **preview**.
- **Framing**: sync is bolt-on and opt-in; a failed sync never breaks the task
  loop.
- **Visual**: none required; a two-card layout or a labelled list is fine.
- **Link**: `/docs/sync`.

### 5.7 Priority, subtasks, blockers, repeats

The "what to do next" section — the everyday value that the current page
barely covers.

- **Copy**: importance + urgency + deadline pressure, so the list sorts itself;
  a task that is not due yet stays out of the way; subtasks, blockers
  (including chains — "this blocks X, which blocks Y"), and ordering hints;
  repeats configured in a picker rather than a syntax.
- **Visual**: `IssueRow`/`taskRows` list and/or `Timeline`/`repeatRows`.
- **Links**: `/docs/tasks`, `/docs/repeats`.
- **§11.2** decides whether this is one section with two halves or two
  sections.

### 5.8 Roadmap teaser

- One short block: a line or two of what is coming, linking to `/roadmap`.
- Must not duplicate the roadmap page's list.

### 5.9 Docs links

- **Slim**: a single quiet block linking into the user guide (`/docs`), with an
  optional secondary link to a specific page. The contributor guide is **not
  featured** (§3.4 #17); it is reachable from the footer.
- No descriptions of the docs site's own structure.

### 5.10 Closing CTA

- `CTASection` with **`Download` → `/downloads`** and **`Read the docs` →
  `/docs`**.
- Title must not be "Start with the guide that fits" (that language is gone).

---

## 6. The `/roadmap` page (normative)

### 6.1 Route and layout

- New page: `apps/docs-web/src/pages/roadmap.astro`.
- Top-level, like `/downloads`: `Base` + `SiteHeader` + `DocsFooter`, with an
  `h1` page header in the `/downloads` style (eyebrow → `h1` → description).
- **Linked from the site header nav**, next to `Docs` and `Download`
  (§3.5 #22). Because `SiteHeader` is shared by `/`, `/downloads`, and
  `/roadmap`, revisit its `aria-current` handling (it currently special-cases
  only the home route).
- Also link it from `DocsFooter` so it is reachable from the docs.

### 6.2 Buckets

| Bucket | Meaning per §3.6 #26 | Contents (draft) |
|--------|----------------------|------------------|
| **Now** | Available today. | Local task list with nested tags, projects and areas; priority scoring; subtasks and blockers; repeat and scheduling; the agent pane; configurable model providers; Todoist two-way sync; travel-checklist mini-app. |
| **Next** | Usable/in-progress, shape still moving. | GitHub issues and projects sync (preview); the git-backed coding workflow (preview); hardening the workflow recipes. |
| **Later** | Specified or sketched. | Message follow-ups and birthday wishes; job-application tracking; outside apps managing part of your lists. |
| **Exploring** | Vague ideas, no commitment (§3.6 #28). | OS-level due reminders; a mobile companion; tag-to-directory binding (from the README); the homepage-feature ideas that are not yet built. |
| **Not planned** | Short, explicit. | Anything multi-user or collaborative. See §11.3. |

- Each entry: one line, plain language, no internal nouns, with a page link
  where one exists.
- Reuse the honesty vocabulary the project already uses, but keep the roadmap's
  own bucket names. Do not re-introduce "available / preview / coming soon" as
  the page's primary axis (§6.3).
- Keep the "the code is the final authority" note from the current status page.

### 6.3 `/docs/status` becomes a stub

- The page stays at its URL as a **one-line stub** linking to `/roadmap`
  (§3.5 #21), inside the `Docs` layout so the guide nav and pager keep
  working.
- Decide whether it stays in the guide nav as "Feature status" or is relabelled
  (§11.5). It must not be a second, competing inventory.
- `src/data/docs.ts` still owns the nav; adjust the label/description there if
  needed.

---

## 7. Data and content changes

### 7.1 `apps/docs-web/data/landing.ts`

- Add **two personal-flavoured rows** to `taskRows` so the one-list section
  demonstrates the positioning (e.g. a dog/household errand and a family
  appointment next to the existing spec/sync rows).
- Keep the existing work-flavoured rows; keep the same key-name discipline
  (the file must stay outside `src/` for Panda CSS scanning).
- `agentThread`, `repeatRows`, `codingColumns`, and the automation records are
  unchanged unless a section needs a different excerpt (§11.4).

### 7.2 `apps/docs-web/src/pages/docs/contributor/architecture.astro`

- Add a **"Tech stack"** section: GPUI, Rust, SQLite, MCP/ACP, and the web
  stack (Astro, Panda CSS, Ark UI) — what each is *for*, and where it lives.
  Absorbs the retired homepage `LogoCloud` (§3.4 #16, §3.6 #29).
- Add the section to that page's `toc`.

### 7.3 `apps/docs-web/data/docs.ts`

- Re-export/keep `REPO_URL` as-is.
- Update the module comment if it still claims every docs route is linked from
  the landing page's guide block.
- Adjust the "Feature status" nav entry per §11.5.

### 7.4 `apps/docs-web/README.md`

- Route table: add `/roadmap`; rewrite the Landing row (no download section, no
  two-guide block); note `/docs/status` is a stub.
- Rewrite the "Structure" paragraph: `/` is the product story, `/downloads` is
  the bundles, `/roadmap` is what's next, everything else is under `/docs`.
- Note the tech-stack section on the architecture page.

### 7.5 Consistency pass

The interview asked for a **whole-docs-site pass** (§3.4 #19). At minimum:

- One vocabulary for *extensions* vs *integrations* across `/`, `/roadmap`,
  `/docs`, `/docs/mini-apps`, `/docs/workflows`, `/docs/sync`, and the README.
- The homepage `<Base>` `description` currently says "for macOS and Linux"
  while Windows bundles now ship (CI publishes a Windows zip). Fix the meta
  description and anywhere else the platform list is stale.
- Remove or relocate any other contributor-facing copy that crept into
  user-facing pages.

---

## 8. Illustration and component inventory

Kept and reused: `Hero`, `StatsBand`, `SectionHeading`, `FeatureSection`,
`CTASection`, `IssueRow`, `Timeline`, `ChatThread`, `AutomationsPanel`,
`Badge`, the `/downloads` page components.

Retired from the homepage: `LogoCloud`, `AnnouncementPill`.

Unused after the change (decide in §11.4): `KanbanBoard` +
`codingColumns`, `BranchCleanup`-shaped automation data if the panel is trimmed.

No new design-system components are required for this change; the new sections
compose from what exists. If the apps & workflows section needs a three-part
layout, prefer `FeatureSection` repeats over a new component.

---

## 9. Tests (`apps/docs-web/tests/docs.spec.ts`)

Existing assertions that the change can break, and what to do:

1. **`landing page › renders without console errors and marks / as current`** —
   asserts the header's `a[aria-current="page"]` is the brand link. Still true;
   re-verify after adding the Roadmap nav entry.
2. **`landing page › links into both guides, and every docs route is reachable
   from it`** — iterates `docsRoutes` and requires a link for each. Today the
   `guides` array plus `DocsFooter` satisfy it. Removing the homepage guides
   block leaves the footer, which links every guide page. Verify; if any route
   becomes unlinked, that route's link belongs in the footer, not back on the
   homepage.
3. **`landing page › the automations pane illustration renders on the landing
   page`** — asserts the text "Branches to clean up" and "Round 2 · Implement"
   are visible and that the tone's background colour reaches the badge. The
   panel survives in §5.5, so this test should keep passing; if the panel's
   mock data changes, update the test's expected strings.
4. **`every documented page lives under /docs`** — `/roadmap` is deliberately
   outside `/docs`, so it must **not** be added to `docsRoutes`. Add it to the
   `routes` array (used by the overflow and axe checks) so it is still covered
   by the layout and accessibility tests.
5. **`the header has one tab per guide and no page-level entries`** — scoped to
   `DocsHeader`; unaffected, but re-run.
6. **`accessibility: WCAG 2.1 AA on every route`** and **overflow** — these
   iterate `routes`; adding `/roadmap` there gives the new page coverage for
   free.

Per §3.4 #18: no new test files; extend the existing suite only. Delete/retire
assertions that can only exist to protect removed blocks.

Verification commands:

```sh
pnpm --dir apps/docs-web run typecheck
pnpm --dir apps/docs-web run build          # must still succeed; expect one more page
pnpm --dir apps/docs-web exec playwright test --workers=1
```

---

## 10. Factual guardrails (from the code, not the interview)

The interview requested no claim restrictions, but the copy still has to be
true. These are the ones that are easy to get wrong:

- **Not notifications/reminders.** `notifications.rs` is an app-wide
  error/warning log plus toasts. Do not promise due-date reminders; if they are
  wanted, they belong in the roadmap's *Exploring* bucket.
- **Not multi-user.** There is no sharing, teams, or collaboration anywhere in
  the code. "One list for your personal tasks and your work tasks" is about
  *you*, not about working with colleagues.
- **Not fully autonomous.** Workflows are explicitly "semi-automated" — the
  agent works inside a stage, and you decide when the stage ends. Keep that.
- **Privacy phrasing.** "No account, no telemetry, works offline" is supported.
  Avoid absolutes like "nothing ever leaves your machine" — Todoist, GitHub,
  and model providers send data when configured.
- **Labels.** GitHub sync and the git-backed coding workflow are **preview**
  per `/docs/status`. Keep the label wherever they are sold.
- **Platforms.** macOS, Linux, and Windows bundles all ship today; the
  macOS/Linux-only phrasing in the current meta description is stale.

---

## 11. Residual decisions (settled)

1. **`StatsBand` link target.** Settled: keep the link, retarget it at
   `/docs/sync#offline` with the label "How sync stays optional".
2. **Priority and repeats: one section or two?** Settled: one section
   ("What to do next"), split into two visual halves — the task list and the
   repeat timeline — each with its own heading and link.
3. **The "Not planned" list.** Settled: include a short not-planned line at the
   bottom of `/roadmap` (team/collaboration features), below *Exploring*.
4. **Is the `KanbanBoard` illustration dropped?** Settled: dropped from the
   homepage. The apps & workflows section uses `AutomationsPanel`, and the
   coding workflow is copy inside it. `codingColumns` stays in
   `data/landing.ts` for the design-system showcase.
5. **The `/docs/status` stub in the guide nav.** Settled: keep the entry under
   its current "Feature status" label pointing at the stub, so the guide's
   nav, reading order, and pager are untouched. The stub carries the link to
   `/roadmap`.
6. **Roadmap ownership.** Settled: entries live in `data/roadmap.ts` (outside
   `src/`, for the Panda scanning reason), rendered by the page.
7. **How far the docs-index pass goes.** Settled: one correction — the
   overview's "Feature status" link now points at `/roadmap` — plus the stale
   macOS/Linux platform list in the homepage meta description. The overview's
   Highlights grid is left alone.
