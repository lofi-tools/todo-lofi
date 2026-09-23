//! Settings panel: tree sub-nav on the left, controls on the right.
//!
//! Tree: General, Sync, Apps (non-leaf with its own controls) with one
//! submenu per app (automation recipe or integration), About. Every leaf —
//! and Apps itself — renders grouped sections of controls. Persisted to
//! `~/.config/my-todo/settings.json` (`MY_TODO_CONFIG_DIR` overrides).

use gpui::{
    AnyElement, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Task, Window, div,
    px, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{Icon, IconName, Sizable, Size, StyledExt};
use std::collections::HashMap;

use crate::store::Store;
use crate::ui_parts::apps::{AppSettings, TagAttachPicker};
use crate::ui_parts::todoist_sync::{TodoistSyncEvent, TodoistSyncPicker};

fn config_dir() -> anyhow::Result<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("MY_TODO_CONFIG_DIR") {
        if !dir.is_empty() {
            return Ok(std::path::PathBuf::from(dir));
        }
    }
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not locate the home directory"))?;
    Ok(home.join(".config").join("my-todo"))
}

fn settings_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(config_dir()?.join("settings.json"))
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    System,
    #[default]
    Dark,
    Light,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct GeneralSettings {
    pub theme: ThemeChoice,
    pub show_completed: bool,
    pub confirm_before_delete: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            show_completed: true,
            confirm_before_delete: true,
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SyncSettings {
    pub auto_sync: bool,
    pub interval_minutes: u32,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            auto_sync: true,
            interval_minutes: 15,
        }
    }
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SettingsFile {
    #[serde(default)]
    pub general: GeneralSettings,
    #[serde(default)]
    pub sync: SyncSettings,
    /// Default capture flag for newly attached tags (Apps node control).
    #[serde(default)]
    pub default_capture_new_tasks: bool,
    /// Per-app slug → notify on run finished.
    #[serde(default)]
    pub app_notify: HashMap<String, bool>,
}

impl SettingsFile {
    pub fn load() -> Self {
        let Ok(path) = settings_path() else {
            return Self::default();
        };
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = settings_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, raw)?;
        Ok(())
    }
}

fn display_name(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
struct AppEntry {
    id: u64,
    slug: String,
    label: String,
    kind: String,
    /// Integration provider (`github`, `todoist`), if this app is an
    /// integration. The nav names the submenu after the provider, not the
    /// account, so the raw `provider-id` slug never shows.
    provider: Option<String>,
    /// Connected account login for integrations, shown as "Connected as …"
    /// on the leaf instead of the account-named submenu.
    account: Option<String>,
    /// Integration id behind an integration app, for per-account queries
    /// such as the GitHub tag mapping.
    integration_id: Option<u64>,
}

/// One line of the GitHub settings mapping: the local tag, the remote repos
/// linked to it, and the tag's local directories.
#[derive(Clone)]
struct GithubMapRow {
    tag_label: String,
    repos: Vec<String>,
    dirs: Vec<String>,
}

/// The nav label for an app: integrations are named after their provider
/// (`Github`), with the account appended only when several accounts of one
/// provider are connected.
fn nav_label(entry: &AppEntry, provider_counts: &HashMap<String, usize>) -> String {
    if entry.kind == "integration"
        && let Some(provider) = entry.provider.as_deref()
    {
        let name = display_name(provider);
        let shared = provider_counts.get(provider).copied().unwrap_or(0) > 1;
        if shared
            && let Some(account) = entry.account.as_deref()
            && !account.is_empty()
        {
            return format!("{name} ({account})");
        }
        return name;
    }
    display_name(&entry.label)
}

pub struct SettingsView {
    store: Store,
    apps: Entity<AppSettings>,
    file: SettingsFile,
    selected: String,
    apps_expanded: bool,
    app_list: Vec<AppEntry>,
    tag_pickers: HashMap<String, Entity<TagAttachPicker>>,
    sync_pickers: HashMap<String, Entity<TodoistSyncPicker>>,
    /// GitHub tag mapping rows per app id, loaded with the app list.
    github_maps: HashMap<u64, Vec<GithubMapRow>>,
    status: Option<String>,
    _load: Option<Task<()>>,
}

impl SettingsView {
    pub fn new(store: Store, apps: Entity<AppSettings>, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            store,
            apps,
            file: SettingsFile::load(),
            selected: "general".to_string(),
            apps_expanded: true,
            app_list: Vec::new(),
            tag_pickers: HashMap::new(),
            sync_pickers: HashMap::new(),
            github_maps: HashMap::new(),
            status: None,
            _load: None,
        };
        view.refresh_apps(cx);
        view
    }

    fn persist(&mut self) {
        if let Err(error) = self.file.save() {
            let message = format!("Could not save settings: {error}");
            // No context here to report with, so the notification layer picks
            // this up from the log line.
            tracing::error!("{message}");
            self.status = Some(message);
        }
    }

    fn refresh_apps(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            let apps = store.list_apps(cx).await.unwrap_or_default();
            // The integration behind each app, so the nav can name the
            // submenu after the provider and the leaf can name the account.
            // The map carries the integration id for per-account queries.
            let mut accounts: HashMap<u64, (String, Option<String>, u64)> = HashMap::new();
            if let Ok(integrations) = store.list_integrations(cx).await {
                for integration in integrations {
                    if let Ok(Some(app)) = store
                        .app_for_integration(integration.id, cx)
                        .await
                    {
                        accounts.insert(
                            app.id,
                            (
                                integration.provider,
                                integration.account_label.filter(|label| !label.is_empty()),
                                integration.id,
                            ),
                        );
                    }
                }
            }
            this.update(cx, |this: &mut Self, cx| {
                this.app_list = apps
                    .iter()
                    .map(|(app, _)| {
                        let (provider, account, integration_id) = accounts
                            .remove(&app.id)
                            .map(|(provider, account, id)| (Some(provider), account, Some(id)))
                            .unwrap_or((None, None, None));
                        AppEntry {
                            id: app.id,
                            slug: app.slug.clone(),
                            label: app.label.clone(),
                            kind: app.kind.clone(),
                            provider,
                            account,
                            integration_id,
                        }
                    })
                    .collect();
                this.app_list.sort_by(|a, b| {
                    Self::nav_sort_key(a).cmp(&Self::nav_sort_key(b))
                });
                this.github_maps
                    .retain(|app_id, _| this.app_list.iter().any(|app| &app.id == app_id));
                this._load = None;
                cx.notify();
            })
            .ok();
            // The GitHub tag mapping loads after the list (it needs the
            // integration ids): a separate task, so a slow map never holds
            // the list load open.
            this.update(cx, |this, cx| this.load_github_maps(cx))
                .ok();
        }));
    }

    /// Fetch the GitHub tag mapping for every connected GitHub account, from
    /// the current app list. Spawned after the list lands; each account's
    /// rows land as they arrive.
    fn load_github_maps(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let github_apps: Vec<(u64, u64)> = self
            .app_list
            .iter()
            .filter(|app| app.kind == "integration" && app.provider.as_deref() == Some("github"))
            .filter_map(|app| app.integration_id.map(|id| (app.id, id)))
            .collect();
        self.github_maps
            .retain(|app_id, _| github_apps.iter().any(|(id, _)| id == app_id));
        cx.spawn(async move |this, cx| {
            for (app_id, integration_id) in github_apps {
                let rows = store
                    .github_tag_map(integration_id, cx)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(tag_label, repos, dirs)| GithubMapRow {
                        tag_label,
                        repos,
                        dirs,
                    })
                    .collect();
                this.update(cx, |this: &mut Self, cx| {
                    this.github_maps.insert(app_id, rows);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Sort key matching the rendered nav label, so the tree stays
    /// alphabetical after integrations take their provider names.
    fn nav_sort_key(entry: &AppEntry) -> String {
        match (&entry.kind as &str, &entry.provider) {
            ("integration", Some(provider)) => display_name(provider),
            _ => display_name(&entry.label),
        }
    }

    fn provider_counts(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for app in &self.app_list {
            if app.kind == "integration"
                && let Some(provider) = app.provider.as_deref()
            {
                *counts.entry(provider.to_string()).or_insert(0) += 1;
            }
        }
        counts
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_apps(cx);
    }

    fn select(&mut self, id: &str, cx: &mut Context<Self>) {
        self.selected = id.to_string();
        // The map is read once per app-list load, which lands before any tag
        // is connected, and repo- or directory-bound tags raise no picker
        // event. Re-read it whenever a GitHub leaf is opened, so the mapping
        // reflects what is connected now.
        if let Some(slug) = id.strip_prefix("app:") {
            let is_github = self.app_list.iter().any(|app| {
                app.slug == slug
                    && app.kind == "integration"
                    && app.provider.as_deref() == Some("github")
            });
            if is_github {
                self.load_github_maps(cx);
            }
        }
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_row()
            .min_h_0()
            .bg(rgb(0x1e1e1e))
            .child(self.nav_column(cx))
            .child(self.controls_column(window, cx))
    }
}

impl SettingsView {
    fn nav_column(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows: Vec<AnyElement> = vec![
            self.nav_row("general", "General", 0, cx),
            self.nav_row("sync", "Sync", 0, cx),
            self.nav_parent_row(cx),
        ];
        let counts = self.provider_counts();
        let app_rows: Vec<(String, String)> = self
            .app_list
            .iter()
            .map(|app| (format!("app:{}", app.slug), nav_label(app, &counts)))
            .collect();
        if self.apps_expanded {
            for (id, label) in &app_rows {
                rows.push(self.nav_row(id, label, 1, cx));
            }
        }
        rows.push(self.nav_row("about", "About", 0, cx));
        div()
            .w(px(220.))
            .flex_none()
            .h_full()
            .overflow_y_scrollbar()
            .border_r_1()
            .border_color(rgb(0x2e2e2e))
            .bg(rgb(0x1a1a1a))
            .p_2()
            .v_flex()
            .gap_0p5()
            .children(rows)
            .into_any_element()
    }

    fn nav_row(&mut self, id: &str, label: &str, depth: u32, cx: &mut Context<Self>) -> AnyElement {
        let active = self.selected == id;
        let id_owned = id.to_string();
        div()
            .id(SharedString::from(format!("settings-nav-{id_owned}")))
            .h_flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .pl(px(8. + depth as f32 * 16.))
            .bg(if active { rgb(0x2a2a2a) } else { rgb(0x1a1a1a) })
            .text_color(if active { rgb(0xffffff) } else { rgb(0xa3a3a3) })
            .hover(|this| this.bg(rgb(0x2a2a2a)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select(&id_owned.clone(), cx);
            }))
            .child(label.to_string())
            .into_any_element()
    }

    fn nav_parent_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.selected == "apps";
        let expanded = self.apps_expanded;
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .bg(if active { rgb(0x2a2a2a) } else { rgb(0x1a1a1a) })
            .text_color(if active { rgb(0xffffff) } else { rgb(0xa3a3a3) })
            .hover(|this| this.bg(rgb(0x2a2a2a)))
            .child(
                div()
                    .id("settings-nav-apps-toggle")
                    .px_1()
                    .text_color(rgb(0x737373))
                    .child(if expanded { "▾" } else { "▸" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.apps_expanded = !this.apps_expanded;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("settings-nav-apps")
                    .flex_1()
                    .child("Apps".to_string())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select("apps", cx);
                    })),
            )
            .into_any_element()
    }

    fn controls_column(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected.clone();
        let body: AnyElement = if selected == "general" {
            self.general_controls(cx)
        } else if selected == "sync" {
            self.sync_controls(cx)
        } else if selected == "apps" {
            self.apps_node_controls(cx)
        } else if let Some(slug) = selected.strip_prefix("app:") {
            self.app_leaf_controls(slug.to_string(), window, cx)
        } else {
            self.about_controls()
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scrollbar()
            .child(div().p_8().v_flex().gap_4().max_w(px(640.)).child(body))
            .into_any_element()
    }

    fn section(title: &str, rows: Vec<AnyElement>) -> AnyElement {
        div()
            .v_flex()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(0x737373))
                    .child(title.to_string()),
            )
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x2e2e2e))
                    .bg(rgb(0x232323))
                    .p_3()
                    .children(rows),
            )
            .into_any_element()
    }

    /// One mapping line per connected tag: the local tag with a folder icon,
    /// the remote repo short url with a GitHub icon, and the tag's local
    /// directories as a vertical sub-list.
    fn github_map(rows: &[GithubMapRow]) -> AnyElement {
        if rows.is_empty() {
            return div()
                .text_xs()
                .text_color(rgb(0x737373))
                .child("No tags connected yet.")
                .into_any_element();
        }
        div()
            .v_flex()
            .gap_1()
            .children(rows.iter().map(|row| {
                let repos: AnyElement = if row.repos.is_empty() {
                    div()
                        .text_xs()
                        .text_color(rgb(0x737373))
                        .child("No repo bound")
                        .into_any_element()
                } else {
                    div()
                        .v_flex()
                        .children(row.repos.iter().map(|repo| {
                            div()
                                .h_flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    Icon::new(
                                        gpui_component_assets::IconName::Github,
                                    )
                                    .with_size(Size::Small),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .text_color(rgb(0xd4d4d4))
                                        .child(repo.clone()),
                                )
                                .into_any_element()
                        }))
                        .into_any_element()
                };
                let dirs: AnyElement = if row.dirs.is_empty() {
                    div()
                        .text_xs()
                        .text_color(rgb(0x737373))
                        .child("No local directories")
                        .into_any_element()
                } else {
                    div()
                        .v_flex()
                        .children(row.dirs.iter().map(|dir| {
                            div()
                                .text_xs()
                                .text_color(rgb(0xd4d4d4))
                                .child(dir.clone())
                                .into_any_element()
                        }))
                        .into_any_element()
                };
                div()
                    .h_flex()
                    .items_start()
                    .gap_3()
                    .py_1()
                    .child(
                        div()
                            .flex_none()
                            .w(px(170.))
                            .h_flex()
                            .items_center()
                            .gap_1p5()
                            .overflow_hidden()
                            .child(Icon::new(IconName::Folder).with_size(Size::Small))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .text_color(rgb(0xd4d4d4))
                                    .child(row.tag_label.clone()),
                            ),
                    )
                    .child(div().flex_1().min_w_0().child(repos))
                    .child(div().flex_1().min_w_0().child(dirs))
                    .into_any_element()
            }))
            .into_any_element()
    }

    fn row(label: &str, hint: Option<&str>, control: AnyElement) -> AnyElement {
        let mut col = div().v_flex().flex_1().gap_0p5().child(
            div()
                .text_sm()
                .text_color(rgb(0xd4d4d4))
                .child(label.to_string()),
        );
        if let Some(hint) = hint {
            col = col.child(
                div()
                    .text_xs()
                    .text_color(rgb(0x737373))
                    .child(hint.to_string()),
            );
        }
        div()
            .h_flex()
            .items_center()
            .gap_3()
            .min_h(px(32.))
            .child(col)
            .child(control)
            .into_any_element()
    }

    fn toggle(
        id: String,
        on: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> AnyElement {
        div()
            .id(SharedString::from(id))
            .px_2()
            .py_0p5()
            .rounded_md()
            .border_1()
            .border_color(if on { rgb(0x4a6fa5) } else { rgb(0x3a3a3a) })
            .bg(if on { rgb(0x2f4057) } else { rgb(0x242424) })
            .text_xs()
            .text_color(if on { rgb(0xdbe6f5) } else { rgb(0xa3a3a3) })
            .hover(|this| this.bg(rgb(0x2a2a2a)))
            .on_click(on_click)
            .child(if on { "On" } else { "Off" })
            .into_any_element()
    }

    fn theme_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.file.general.theme;
        let chip = |id: &str, label: &str, active: bool, value: ThemeChoice, cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(id.to_string()))
                .px_2()
                .py_0p5()
                .rounded_md()
                .border_1()
                .border_color(if active { rgb(0x4a6fa5) } else { rgb(0x3a3a3a) })
                .bg(if active { rgb(0x2f4057) } else { rgb(0x242424) })
                .text_xs()
                .text_color(if active { rgb(0xdbe6f5) } else { rgb(0xa3a3a3) })
                .hover(|this| this.bg(rgb(0x2a2a2a)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.file.general.theme = value;
                    this.persist();
                    cx.notify();
                }))
                .child(label.to_string())
                .into_any_element()
        };
        let row = div().h_flex().items_center().gap_1().child(chip(
            "settings-theme-system",
            "System",
            theme == ThemeChoice::System,
            ThemeChoice::System,
            cx,
        ));
        let row = row.child(chip(
            "settings-theme-dark",
            "Dark",
            theme == ThemeChoice::Dark,
            ThemeChoice::Dark,
            cx,
        ));
        let row = row.child(chip(
            "settings-theme-light",
            "Light",
            theme == ThemeChoice::Light,
            ThemeChoice::Light,
            cx,
        ));
        Self::row(
            "Theme",
            Some("Controls the app chrome tone."),
            row.into_any_element(),
        )
    }

    fn stepper(&mut self, id_prefix: &str, value: u32, cx: &mut Context<Self>) -> AnyElement {
        let minus_id = format!("{id_prefix}-minus");
        let plus_id = format!("{id_prefix}-plus");
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .child(
                Button::new(minus_id)
                    .ghost()
                    .compact()
                    .with_size(Size::Small)
                    .label("−")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.file.sync.interval_minutes =
                            (this.file.sync.interval_minutes.saturating_sub(5)).max(5);
                        this.persist();
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0xd4d4d4))
                    .child(format!("{value} min")),
            )
            .child(
                Button::new(plus_id)
                    .ghost()
                    .compact()
                    .with_size(Size::Small)
                    .label("+")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.file.sync.interval_minutes =
                            (this.file.sync.interval_minutes + 5).min(240);
                        this.persist();
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn general_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let appearance = Self::section("Appearance", vec![self.theme_row(cx)]);
        let show_completed = self.file.general.show_completed;
        let confirm = self.file.general.confirm_before_delete;
        let tasks = Self::section(
            "Tasks",
            vec![
                Self::row(
                    "Show completed",
                    Some("Keep done items visible in lists."),
                    Self::toggle(
                        "settings-show-completed".to_string(),
                        show_completed,
                        cx.listener(|this, _, _, cx| {
                            this.file.general.show_completed = !this.file.general.show_completed;
                            this.persist();
                            cx.notify();
                        }),
                    ),
                ),
                Self::row(
                    "Confirm before delete",
                    Some("Ask before destructive removes."),
                    Self::toggle(
                        "settings-confirm-delete".to_string(),
                        confirm,
                        cx.listener(|this, _, _, cx| {
                            this.file.general.confirm_before_delete =
                                !this.file.general.confirm_before_delete;
                            this.persist();
                            cx.notify();
                        }),
                    ),
                ),
            ],
        );
        div()
            .v_flex()
            .gap_4()
            .child(div().text_xl().font_semibold().child("General"))
            .child(appearance)
            .child(tasks)
            .into_any_element()
    }

    fn sync_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let auto = self.file.sync.auto_sync;
        let interval = self.file.sync.interval_minutes;
        div()
            .v_flex()
            .gap_4()
            .child(div().text_xl().font_semibold().child("Sync"))
            .child(Self::section(
                "Automatic sync",
                vec![
                    Self::row(
                        "Auto sync",
                        Some("Sync integrations on an interval."),
                        Self::toggle(
                            "settings-auto-sync".to_string(),
                            auto,
                            cx.listener(|this, _, _, cx| {
                                this.file.sync.auto_sync = !this.file.sync.auto_sync;
                                this.persist();
                                cx.notify();
                            }),
                        ),
                    ),
                    Self::row(
                        "Interval",
                        Some("How often auto sync runs."),
                        self.stepper("settings-interval", interval, cx),
                    ),
                ],
            ))
            .into_any_element()
    }

    fn apps_node_controls(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let capture = self.file.default_capture_new_tasks;
        let count = self.app_list.len();
        div()
            .v_flex()
            .gap_4()
            .child(div().text_xl().font_semibold().child("Apps"))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0xa3a3a3))
                    .child(format!(
                        "{count} app(s) installed. Pick one on the left for its own settings."
                    )),
            )
            .child(Self::section(
                "Defaults",
                vec![Self::row(
                    "New tags capture tasks",
                    Some("Default when attaching an app to a tag."),
                    Self::toggle(
                        "settings-default-capture".to_string(),
                        capture,
                        cx.listener(|this, _, _, cx| {
                            this.file.default_capture_new_tasks =
                                !this.file.default_capture_new_tasks;
                            this.persist();
                            cx.notify();
                        }),
                    ),
                )],
            ))
            .into_any_element()
    }

    fn app_leaf_controls(
        &mut self,
        slug: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = self.app_list.iter().find(|app| app.slug == slug);
        let Some(entry) = entry else {
            return div()
                .text_sm()
                .text_color(rgb(0xa3a3a3))
                .child("Unknown app.")
                .into_any_element();
        };
        let app_id = entry.id;
        let kind = entry.kind.clone();
        // Integrations are titled with the provider; the connected account
        // reads as "Connected as …" underneath instead of naming the submenu.
        let (label, subtitle) = if kind == "integration"
            && let Some(provider) = entry.provider.as_deref()
        {
            let title = display_name(provider);
            let subtitle = match entry.account.as_deref().filter(|name| !name.is_empty()) {
                Some(account) => format!("Connected as {account}"),
                None => format!("{} app", display_name(&kind)),
            };
            (title, subtitle)
        } else {
            (
                display_name(&entry.label),
                format!("{} app · {}", display_name(&kind), display_name(&slug)),
            )
        };
        let is_demo = slug == "demo" || kind == "builtin";
        let is_integration = kind == "integration";
        let notify = self.file.app_notify.get(&slug).copied().unwrap_or(true);
        let slug_for_toggle = slug.clone();
        let header = div()
            .v_flex()
            .gap_0p5()
            .child(div().text_xl().font_semibold().child(label.clone()))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0xa3a3a3))
                    .child(subtitle),
            );
        let preferences = Self::section(
            "Preferences",
            vec![Self::row(
                "Notify on run finished",
                Some("Surface a status note when this app finishes work."),
                Self::toggle(
                    format!("settings-notify-{slug}"),
                    notify,
                    cx.listener(move |this, _, _, cx| {
                        let next = !this
                            .file
                            .app_notify
                            .get(&slug_for_toggle)
                            .copied()
                            .unwrap_or(true);
                        this.file.app_notify.insert(slug_for_toggle.clone(), next);
                        this.persist();
                        cx.notify();
                    }),
                ),
            )],
        );
        let mut page = div().v_flex().gap_4().child(header).child(preferences);
        if is_demo {
            return page
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xa3a3a3))
                        .child("Demo content needs no tag or run settings."),
                )
                .into_any_element();
        }
        if is_integration {
            if slug.starts_with("todoist") {
                let picker_id = format!("settings-sync-{slug}");
                let picker = self.sync_picker(&slug, window, cx);
                let disabled = self
                    .apps
                    .read_with(cx, |apps, _| apps.app(app_id).is_some_and(|app| !app.enabled));
                let mut rows = vec![
                    div()
                        .text_xs()
                        .text_color(rgb(0x737373))
                        .child(if disabled {
                            "Todoist is disabled. Re-enable it on the integrations card to resume syncing."
                        } else {
                            "Which Todoist projects sync into which local tags. \
                             Pick a project first, then pair it with a tag."
                        })
                        .into_any_element(),
                ];
                if !disabled {
                    rows.push(picker.update(cx, |picker, cx| {
                        picker.render_settings_block(&picker_id, window, cx)
                    }));
                }
                page = page.child(Self::section("Synced tags", rows));
            } else {
                let is_github = entry.provider.as_deref() == Some("github");
                let github_rows = is_github
                    .then(|| self.github_maps.get(&app_id).cloned().unwrap_or_default());
                let picker_id = format!("settings-tags-{slug}");
                let picker = self.tag_picker(&slug, app_id, true, window, cx);
                let mut synced = vec![
                    div()
                        .text_xs()
                        .text_color(rgb(0x737373))
                        .child("Tags this integration syncs. Type to find one, Enter to attach.")
                        .into_any_element(),
                    picker.update(cx, |picker, cx| {
                        picker.render_picker(&picker_id, window, cx)
                    }),
                ];
                // GitHub names its remote per tag, so the leaf maps each
                // connected tag to its repos and local directories instead
                // of repeating the plain tag list below.
                if let Some(rows) = github_rows {
                    synced.push(Self::github_map(&rows));
                }
                page = page.child(Self::section("Synced tags", synced));
            }
        }
        let block = self.apps.update(cx, |settings, cx| {
            settings.settings_block(app_id, 1, cx)
        });
        page
            .child(Self::section("Tags & runs", vec![block]))
            .into_any_element()
    }

    /// Lazily created fuzzy tag picker for one app, refreshing the app list
    /// when its attachments change.
    fn tag_picker(
        &mut self,
        slug: &str,
        app_id: u64,
        capture_default: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TagAttachPicker> {
        if let Some(picker) = self.tag_pickers.get(slug) {
            return picker.clone();
        }
        let store = self.store.clone();
        let apps = self.apps.clone();
        let picker = cx.new(|cx| TagAttachPicker::new(store, app_id, capture_default, window, cx));
        cx.subscribe(&picker, move |this, _picker, event, cx| match event {
            crate::ui_parts::apps::TagAttachEvent::Changed => {
                apps.update(cx, |settings, cx| settings.refresh(cx));
                this.refresh(cx);
            }
        })
        .detach();
        self.tag_pickers.insert(slug.to_string(), picker.clone());
        picker
    }

    /// Lazily created Todoist project ↔ tag pairing picker, refreshing the
    /// app list when pairs change (syncs create tags and sections).
    fn sync_picker(
        &mut self,
        slug: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TodoistSyncPicker> {
        if let Some(picker) = self.sync_pickers.get(slug) {
            return picker.clone();
        }
        let store = self.store.clone();
        let apps = self.apps.clone();
        let picker = cx.new(|cx| TodoistSyncPicker::new(store, window, cx));
        cx.subscribe(&picker, move |this, _picker, event, cx| match event {
            TodoistSyncEvent::Changed => {
                apps.update(cx, |settings, cx| settings.refresh(cx));
                this.refresh(cx);
            }
        })
        .detach();
        self.sync_pickers.insert(slug.to_string(), picker.clone());
        picker
    }

    fn about_controls(&self) -> AnyElement {
        div()
            .v_flex()
            .gap_4()
            .child(div().text_xl().font_semibold().child("About"))
            .child(Self::section(
                "App",
                vec![Self::row(
                    "Config location",
                    Some("General, sync, and per-app flags live here."),
                    div()
                        .text_xs()
                        .text_color(rgb(0xa3a3a3))
                        .child("~/.config/my-todo/settings.json")
                        .into_any_element(),
                )],
            ))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, label: &str, provider: Option<&str>, account: Option<&str>) -> AppEntry {
        AppEntry {
            id: 1,
            slug: "github-2".to_string(),
            label: label.to_string(),
            kind: kind.to_string(),
            provider: provider.map(str::to_string),
            account: account.map(str::to_string),
            integration_id: None,
        }
    }

    #[test]
    fn integration_submenus_use_the_provider_name() {
        let counts = HashMap::new();
        let github = entry("integration", "nmrshll", Some("github"), Some("nmrshll"));
        assert_eq!(nav_label(&github, &counts), "Github");
    }

    #[test]
    fn repeated_provider_accounts_are_disambiguated() {
        let counts = HashMap::from([("github".to_string(), 2)]);
        let github = entry("integration", "nmrshll", Some("github"), Some("nmrshll"));
        assert_eq!(nav_label(&github, &counts), "Github (nmrshll)");
    }

    #[test]
    fn non_integrations_keep_their_label() {
        let counts = HashMap::new();
        let recipe = entry("recipe", "coding-task", None, None);
        assert_eq!(nav_label(&recipe, &counts), "Coding-task");
    }

    /// `refresh_apps` fills the GitHub map for the integration's app: a tag
    /// attached through the picker (binding only) still yields a row. Pumps
    /// the executor until the background loads land.
    #[gpui::test]
    fn refresh_apps_loads_the_github_map(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("the test's Tokio runtime");
        let handle = runtime.handle().clone();
        cx.update(|cx| gpui_tokio::init_from_handle(cx, handle.clone()));
        let store = handle.block_on(async {
            let config = storage::StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            let mut store = storage::TodoStore::new(&config)
                .await
                .expect("the in-memory store");
            let integration = store
                .create_integration("github", Some("nmrshll".to_string()))
                .await
                .expect("the integration");
            let app = store
                .app_for_integration(integration.id)
                .await
                .expect("the integration's app")
                .expect("an app");
            let tag = store.create_tag("todo-lofi").await.expect("the tag");
            store
                .attach_app_to_tag(app.id, tag.id, storage::BindingRole::Partial, false)
                .await
                .expect("the picker attachment");
            Store::new(store)
        });
        let view = cx.update(|cx| {
            let apps = cx.new(|cx| crate::ui_parts::apps::AppSettings::new(store.clone(), cx));
            cx.new(|cx| SettingsView::new(store.clone(), apps, cx))
        });
        cx.executor().allow_parking();
        for _ in 0..400 {
            let has_row = cx.update(|cx| {
                view.read(cx)
                    .github_maps
                    .values()
                    .flatten()
                    .any(|row| row.tag_label == "todo-lofi")
            });
            if has_row {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            cx.run_until_parked();
        }
        panic!("the github map never loaded");
    }

    /// Opening a GitHub leaf re-reads the mapping. The app-list load runs
    /// before any tag is connected and repo-bound tags raise no picker event,
    /// so without a reload on selection the mapping stayed empty.
    #[gpui::test]
    fn selecting_the_github_leaf_reloads_the_tag_map(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("the test's Tokio runtime");
        let handle = runtime.handle().clone();
        cx.update(|cx| gpui_tokio::init_from_handle(cx, handle.clone()));
        let (store, app_id, tag_id, slug) = handle.block_on(async {
            let config = storage::StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            let mut store = storage::TodoStore::new(&config)
                .await
                .expect("the in-memory store");
            let integration = store
                .create_integration("github", Some("octocat".to_string()))
                .await
                .expect("the integration");
            let app = store
                .app_for_integration(integration.id)
                .await
                .expect("the integration's app")
                .expect("an app");
            let tag = store.create_tag("later-bound").await.expect("the tag");
            (Store::new(store), app.id, tag.id, app.slug)
        });
        let view = cx.update(|cx| {
            let apps = cx.new(|cx| crate::ui_parts::apps::AppSettings::new(store.clone(), cx));
            cx.new(|cx| SettingsView::new(store.clone(), apps, cx))
        });
        cx.executor().allow_parking();
        // The first load lands with nothing connected yet.
        for _ in 0..400 {
            if cx.update(|cx| view.read(cx).github_maps.contains_key(&app_id)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            cx.run_until_parked();
        }
        let initial_rows = cx.update(|cx| {
            view.read(cx)
                .github_maps
                .get(&app_id)
                .map(Vec::len)
                .unwrap_or(0)
        });
        assert_eq!(initial_rows, 0, "nothing is connected before the tag is bound");

        // Bind the tag after that load, the way repo detection does: no
        // picker event announces it, so only re-opening the leaf refreshes it.
        let attached = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = attached.clone();
        cx.spawn(move |cx: gpui::AsyncApp| async move {
            if store
                .attach_app_to_tag(app_id, tag_id, storage::BindingRole::Partial, false, &cx)
                .await
                .is_ok()
            {
                flag.set(true);
            }
        })
        .detach();
        for _ in 0..400 {
            if attached.get() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            cx.run_until_parked();
        }
        assert!(attached.get(), "the tag attachment never landed");

        cx.update(|cx| {
            view.update(cx, |view, cx| view.select(&format!("app:{slug}"), cx));
        });
        for _ in 0..400 {
            let has_row = cx.update(|cx| {
                view.read(cx)
                    .github_maps
                    .get(&app_id)
                    .is_some_and(|rows| rows.iter().any(|row| row.tag_label == "later-bound"))
            });
            if has_row {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            cx.run_until_parked();
        }
        panic!("re-opening the github leaf did not reload the tag map");
    }

    /// The mapping unions both connection shapes: a repo-bound tag, a tag
    /// attached through the picker (binding only, no repo), and a tag with
    /// only a sync target. The store reads run on Tokio while the answer
    /// comes back on a GPUI task, so the test pumps until it lands.
    #[gpui::test]
    fn the_github_map_unions_bindings_and_repos(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("the test's Tokio runtime");
        let handle = runtime.handle().clone();
        cx.update(|cx| gpui_tokio::init_from_handle(cx, handle.clone()));
        let (store, integration_id) = handle.block_on(async {
            let config = storage::StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            let mut store = storage::TodoStore::new(&config)
                .await
                .expect("the in-memory store");
            let integration = store
                .create_integration("github", Some("octocat".to_string()))
                .await
                .expect("the integration");
            let app = store
                .app_for_integration(integration.id)
                .await
                .expect("the integration's app")
                .expect("an app");
            let bound = store.create_tag("bound").await.expect("the repo tag");
            store
                .bind_repo_tag(bound.id, integration.id, "octocat", "hello-world")
                .await
                .expect("the repo binding");
            let attached = store.create_tag("attached").await.expect("the picker tag");
            store
                .attach_app_to_tag(
                    app.id,
                    attached.id,
                    storage::BindingRole::Partial,
                    false,
                )
                .await
                .expect("the picker attachment");
            let targeted = store.create_tag("targeted").await.expect("the sync tag");
            store
                .set_tag_sync_target(
                    targeted.id,
                    Some(storage::SyncTarget {
                        integration_id: integration.id,
                        external_id: "octocat/solo".to_string(),
                    }),
                )
                .await
                .expect("the sync target");
            store
                .set_tag_dirs(bound.id, vec!["/repos/hello-world".to_string()])
                .await
                .expect("the directories");
            (Store::new(store), integration.id)
        });

        cx.executor().allow_parking();
        let rows = std::rc::Rc::new(std::cell::Cell::new(None));
        let recorder = rows.clone();
        let store = store.clone();
        cx.spawn(move |cx: gpui::AsyncApp| async move {
            let map = store.github_tag_map(integration_id, &cx).await.ok();
            recorder.set(map);
        })
        .detach();
        for _ in 0..400 {
            if let Some(map) = rows.take() {
                assert_eq!(map.len(), 3);
                let bound = map.iter().find(|(label, _, _)| label == "bound").expect("bound");
                assert_eq!(bound.1, vec!["octocat/hello-world".to_string()]);
                assert_eq!(bound.2, vec!["/repos/hello-world".to_string()]);
                let attached = map.iter().find(|(label, _, _)| label == "attached").expect("attached");
                assert!(attached.1.is_empty());
                let targeted = map.iter().find(|(label, _, _)| label == "targeted").expect("targeted");
                assert_eq!(targeted.1, vec!["octocat/solo".to_string()]);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            cx.run_until_parked();
        }
        panic!("the tag map did not finish");
    }
}
