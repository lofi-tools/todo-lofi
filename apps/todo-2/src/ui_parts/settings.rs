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
use gpui_component::{Sizable, Size, StyledExt};
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
            status: None,
            _load: None,
        };
        view.refresh_apps(cx);
        view
    }

    fn persist(&mut self) {
        if let Err(error) = self.file.save() {
            self.status = Some(format!("Could not save settings: {error}"));
        }
    }

    fn refresh_apps(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            let apps = store.list_apps(cx).await.unwrap_or_default();
            this.update(cx, |this: &mut Self, cx| {
                this.app_list = apps
                    .iter()
                    .map(|(app, _)| AppEntry {
                        id: app.id,
                        slug: app.slug.clone(),
                        label: app.label.clone(),
                        kind: app.kind.clone(),
                    })
                    .collect();
                this.app_list.sort_by(|a, b| a.label.cmp(&b.label));
                this._load = None;
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.refresh_apps(cx);
    }

    fn select(&mut self, id: &str, cx: &mut Context<Self>) {
        self.selected = id.to_string();
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
        let app_rows: Vec<(String, String)> = self
            .app_list
            .iter()
            .map(|app| (format!("app:{}", app.slug), display_name(&app.label)))
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
        let label = display_name(&entry.label);
        let kind = entry.kind.clone();
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
                    .child(format!("{} app · {}", display_name(&kind), display_name(&slug))),
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
                page = page.child(Self::section(
                    "Synced tags",
                    vec![
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child(
                                "Which Todoist projects sync into which local tags. \
                                 Pick a project first, then pair it with a tag.",
                            )
                            .into_any_element(),
                        picker.update(cx, |picker, cx| {
                            picker.render_settings_block(&picker_id, window, cx)
                        }),
                    ],
                ));
            } else {
                let picker_id = format!("settings-tags-{slug}");
                let picker = self.tag_picker(&slug, app_id, true, window, cx);
                page = page.child(Self::section(
                    "Synced tags",
                    vec![
                        div()
                            .text_xs()
                            .text_color(rgb(0x737373))
                            .child("Tags this integration syncs. Type to find one, Enter to attach.")
                            .into_any_element(),
                        picker.update(cx, |picker, cx| {
                            picker.render_picker(&picker_id, window, cx)
                        }),
                    ],
                ));
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
