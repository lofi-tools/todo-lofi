//! Tag settings popover: what a tag is placed under, the directories backing
//! it, its sections, and the apps attached to it.
//!
//! Opened from the gear beside the tag title in the task list, or from a tag
//! row's context menu in the navbar, and painted by the layout as its popover
//! slot so it sits above the pane content (paint order follows tree order;
//! there is no z-index).
//!
//! Every action writes immediately — there is no staged edit and no revert.
//! The one exception is removing a placement, which asks first through the
//! window's dialog because it moves a whole subtree out of the nav.

use gpui::{
    AnyElement, AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, StatefulInteractiveElement, Styled, Subscription, Task, Window, deferred, div,
    prelude::FluentBuilder, px, relative, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::switch::Switch;
use gpui_component::{Disableable, IconName, Size, Sizable, StyledExt, WindowExt};
use std::collections::HashSet;
use std::path::PathBuf;
use storage::prelude::*;

use super::apps::rank_tag;
use super::task_details::{TAG_EDITOR_CONTEXT, TagConfirmText, TagSuggestNext, TagSuggestPrev};
use crate::store::Store;
use crate::theme::{APP_BG, CARD_BG, HAIRLINE, PANEL_BG, TEXT_MUTED};

#[derive(Clone)]
pub enum TagSettingsEvent {
    /// A placement, directory, section, or binding changed, so the nav tree
    /// and the task list's sections must be re-read.
    Changed,
}

pub struct TagSettingsPanel {
    store: Store,
    open: bool,
    tag: Option<Tag>,
    /// Tags this tag is placed under (the editor's committed value).
    parents: Vec<Tag>,
    dirs: Vec<String>,
    /// The remote object this tag is bound to, when it is synced (a GitHub
    /// `owner/repo` today), shown beside the directories and changeable there.
    sync_target: Option<storage::SyncTarget>,
    /// Every `owner/repo` this tag syncs with: the repository list.
    bound_repos: Vec<String>,
    /// The `owner/repo` the tag's directories currently resolve to, shown
    /// when nothing is bound yet: what the next sync would detect (§5.3).
    detected_repo: Option<String>,
    _detect: Option<Task<()>>,
    /// Whether each worktree of a run builds into its own `target/` instead of
    /// sharing the repo's build cache (spec §6.4).
    isolated_build_cache: bool,
    sections: Vec<TagSection>,
    bindings: Vec<(App, AppTagBinding)>,
    /// The staged parent-tag editor: desired parent labels as chips, edited
    /// with the same inline chip+input+recommendation UI as a task's tags.
    /// Nothing writes until the draft is committed on Enter (empty text) or
    /// blur.
    placements_draft: Vec<String>,
    placements_input: Entity<InputState>,
    _placements_input_sub: Subscription,
    /// Every tag (id, label) for the parent suggestions.
    all_tags: Vec<(u64, String)>,
    placement_suggest_cursor: usize,
    placement_suggest_active: bool,
    /// The editor's draft reflects the freshly-fetched parents rather than an
    /// in-flight staged edit.
    placements_reload: bool,
    pending_placement_input_clear: bool,
    /// Tags that may not be chosen as a parent: this tag and its descendants
    /// (the store would reject a cycle).
    blocked: HashSet<u64>,
    /// Directories offered for this tag, from the home-directory scan.
    dir_candidates: Vec<PathBuf>,
    /// The "+" directory-picker fuzzy search.
    dir_picker_open: bool,
    dir_picker_input: Entity<InputState>,
    _dir_picker_sub: Subscription,
    dir_picker_cursor: usize,
    /// The connected GitHub integration, when there is one: without it there is
    /// nothing to bind to, so the section is not rendered.
    github_integration_id: Option<u64>,
    /// The repo picker: its open state, its query, and the account's
    /// repositories, fetched the first time it opens.
    repo_picker_open: bool,
    repo_picker_input: Entity<InputState>,
    _repo_picker_sub: Subscription,
    _repo_fetch: Option<Task<()>>,
    repo_picker_cursor: usize,
    repo_candidates: Vec<(String, bool)>,
    repo_candidates_loading: bool,
    repo_candidates_error: Option<String>,
    section_name: Entity<InputState>,
    /// One-line result of the last action, so writes are visible.
    notice: Option<String>,
    _fetch: Option<Task<()>>,
}

impl EventEmitter<TagSettingsEvent> for TagSettingsPanel {}

impl TagSettingsPanel {
    pub fn new(store: Store, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placements_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Add parent tag…", window, cx);
            state
        });
        let _placements_input_sub = cx.subscribe(&placements_input, |this, _, event, cx| match event {
            InputEvent::PressEnter { .. } => this.flush_placement_pending(cx),
            InputEvent::Change => cx.notify(),
            InputEvent::Blur => this.commit_placements(cx),
            _ => {}
        });
let dir_picker_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Search directories…", window, cx);
            state
        });
        let _dir_picker_sub = cx.subscribe(&dir_picker_input, |this, _, event, cx| match event {
            InputEvent::PressEnter { .. } => this.on_dir_picker_enter(cx),
            InputEvent::Change => {
                this.dir_picker_cursor = 0;
                cx.notify();
            }
            _ => {}
        });
        let repo_picker_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Search repositories…", window, cx);
            state
        });
        let _repo_picker_sub = cx.subscribe(&repo_picker_input, |this, _, event, cx| match event {
            InputEvent::PressEnter { .. } => this.on_repo_picker_enter(cx),
            InputEvent::Change => {
                this.repo_picker_cursor = 0;
                cx.notify();
            }
            _ => {}
        });
        Self {
            store,
            open: false,
            tag: None,
            parents: Vec::new(),
            dirs: Vec::new(),
            sync_target: None,
            bound_repos: Vec::new(),
            detected_repo: None,
            _detect: None,
            isolated_build_cache: false,
            sections: Vec::new(),
            bindings: Vec::new(),
            placements_draft: Vec::new(),
            placements_input,
            _placements_input_sub,
            all_tags: Vec::new(),
            placement_suggest_cursor: 0,
            placement_suggest_active: false,
            placements_reload: true,
            pending_placement_input_clear: false,
            blocked: HashSet::new(),
            dir_candidates: Vec::new(),
            dir_picker_open: false,
            dir_picker_input,
            _dir_picker_sub,
            dir_picker_cursor: 0,
            github_integration_id: None,
            repo_picker_open: false,
            repo_picker_input,
            _repo_picker_sub,
            _repo_fetch: None,
            repo_picker_cursor: 0,
            repo_candidates: Vec::new(),
            repo_candidates_loading: false,
            repo_candidates_error: None,
            section_name: cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_placeholder("New section", window, cx);
                input
            }),
            notice: None,
            _fetch: None,
        }
    }

    /// Whether the popover is open (the window-wide Escape observer closes it
    /// before anything else reacts).
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open the popover for `tag_name`, or retarget it when it is already up.
    /// `dir_candidates` are the directories the project scan found.
    pub fn open(&mut self, tag_name: String, dir_candidates: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.open = true;
        self.dir_candidates = dir_candidates;
        self.notice = None;
        // Staged parent edits never outlive the popover: re-opening rebuilds
        // the draft from the store's current parents instead.
        self.placements_reload = true;
        self.pending_placement_input_clear = true;
        let lookup = self.store.get_tag_by_name(tag_name, cx);
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let tag = match lookup.await {
                Ok(Some(tag)) => tag,
                Ok(None) => {
                    tracing::warn!("tag settings: no such tag");
                    return;
                }
                Err(error) => {
                    tracing::error!(%error, "tag settings: could not load the tag");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                this.tag = Some(tag);
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            cx.notify();
        }
    }

    /// Re-read everything the panel shows.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let store = self.store.clone();
        let tag_id = tag.id;
        let parents = store.tag_parents(tag_id, cx);
        let dirs = store.tag_dirs(tag_id, cx);
        let sync_target = store.tag_sync_target(tag_id, cx);
        let isolated_build_cache = store.tag_isolated_build_cache(tag_id, cx);
        let integrations = store.list_integrations(cx);
        let sections = store.tag_sections(tag_id, cx);
        let bindings = store.bindings_for_tag(tag_id, cx);
        let descendants = store.tag_descendant_ids(tag_id, cx);
        let tags = store.list_tags(cx);
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let parents = parents.await.unwrap_or_default();
            let dirs = dirs.await.unwrap_or_default();
            let sync_target = sync_target.await.ok().flatten();
            let isolated_build_cache = isolated_build_cache.await.unwrap_or(false);
            let github_integration_id = integrations
                .await
                .unwrap_or_default()
                .into_iter()
                .find(|integration| integration.provider == "github")
                .map(|integration| integration.id);
            let bound_repos = match github_integration_id {
                Some(integration_id) => store
                    .tag_bound_repos(tag_id, integration_id, cx)
                    .await
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            let sections = sections.await.unwrap_or_default();
            let bindings = bindings.await.unwrap_or_default();
            let blocked = descendants.await.unwrap_or_default();
            let mut all_tags: Vec<(u64, String)> = tags
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|tag| (tag.id, tag.label()))
                .collect();
            all_tags.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
            this.update(cx, |this, cx| {
                this.parents = parents;
                this.dirs = dirs;
                this.sync_target = sync_target;
                this.bound_repos = bound_repos;
                this.isolated_build_cache = isolated_build_cache;
                this.github_integration_id = github_integration_id;
                this.sections = sections;
                this.bindings = bindings;
                this.blocked = blocked.into_iter().collect();
                this.all_tags = all_tags;
                if this.placements_reload {
                    this.placements_reload = false;
                    this.placements_draft = this.parents.iter().map(|parent| parent.label()).collect();
                    this.placement_suggest_cursor = 0;
                    this.placement_suggest_active = false;
                }
                this._fetch = None;
                this.detect_repo(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Run a write, then re-read the panel and tell the world the tree moved.
    fn run(&mut self, action: Task<anyhow::Result<()>>, note: &str, cx: &mut Context<Self>) {
        let note = note.to_string();
        cx.spawn(async move |this, cx| {
            let outcome = action.await;
            this.update(cx, |this, cx| {
                this.notice = Some(match outcome {
                    Ok(()) => {
                        this.refresh(cx);
                        cx.emit(TagSettingsEvent::Changed);
                        note
                    }
                    Err(error) => format!("{error}"),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run a write and re-read without the one-line notice: for controls
    /// whose state is visible on the control itself, an echo at the card
    /// bottom restating it is just noise. Failures still surface.
    fn run_quiet(&mut self, action: Task<anyhow::Result<()>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let outcome = action.await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(()) => {
                        this.refresh(cx);
                        cx.emit(TagSettingsEvent::Changed);
                    }
                    Err(error) => {
                        this.notice = Some(format!("{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Apply the staged parent list: resolve each label to its tag in the same
    /// way a task's tags are stored, then diff against the current parents.
    fn commit_placements(&mut self, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let draft: Vec<String> = self.placements_draft.clone();
        let parents: Vec<String> = self.parents.iter().map(|parent| parent.label()).collect();
        let differs = draft.len() != parents.len()
            || draft
                .iter()
                .zip(parents.iter())
                .any(|(a, b)| a.to_lowercase() != b.to_lowercase());
        if !differs {
            return;
        }
        // Rebuild the draft from the store's canonical labels once the write
        // lands, and restart with an empty field while we are at it.
        self.placements_reload = true;
        self.pending_placement_input_clear = true;
        let action = self.store.set_tag_parents(tag.id, draft, cx);
        self.run(action, "Placed under the updated tags.", cx);
    }

    /// Enter in the parent-tag field: with a keyboard-highlighted suggestion it
    /// adds that tag; otherwise the typed text becomes a chip, unless it is
    /// empty — which commits the staged list.
    fn on_placement_input_enter(&mut self, cx: &mut Context<Self>) {
        if self.placement_suggest_active {
            self.complete_suggestion(cx);
            return;
        }
        let text = self.placements_input.read(cx).text().to_string();
        if text.trim().is_empty() {
            self.commit_placements(cx);
        } else {
            self.flush_placement_pending(cx);
        }
    }

    fn flush_placement_pending(&mut self, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let text = self.placements_input.read(cx).text().to_string();
        let text = text.trim().to_string();
        if text.is_empty() {
            self.commit_placements(cx);
            return;
        }
        self.pending_placement_input_clear = true;
        if !self
            .placements_draft
            .iter()
            .any(|label| label.eq_ignore_ascii_case(&text))
        {
            let blocked = self.all_tags.iter().any(|(id, label)| {
                (*id == tag.id || self.blocked.contains(id))
                    && label.eq_ignore_ascii_case(&text)
            });
            if blocked {
                self.notice =
                    Some("A tag can't be placed under itself or its own child.".to_string());
            } else {
                self.placements_draft.push(text);
            }
        }
        self.placement_suggest_cursor = 0;
        self.placement_suggest_active = false;
        cx.notify();
    }

    fn complete_suggestion(&mut self, cx: &mut Context<Self>) {
        let query = self.placements_input.read(cx).text().to_string();
        let suggestions = self.placement_suggestions(&query);
        if let Some(label) = suggestions
            .get(self.placement_suggest_cursor)
            .map(|(_, label)| label.clone())
        {
            self.push_placement_label(label);
            self.pending_placement_input_clear = true;
        }
        self.placement_suggest_active = false;
        cx.notify();
    }

    fn move_placement_suggestion(&mut self, delta: isize, cx: &mut Context<Self>) {
        let query = self.placements_input.read(cx).text().to_string();
        let count = self.placement_suggestions(&query).len();
        if count == 0 {
            return;
        }
        self.placement_suggest_cursor =
            (self.placement_suggest_cursor as isize + delta).rem_euclid(count as isize) as usize;
        self.placement_suggest_active = true;
        cx.notify();
    }

    /// Existing tags first: exclude this tag and anything under it (the store
    /// forbids cycles) and labels already staged.
    fn placement_suggestions(&self, query: &str) -> Vec<(u64, String)> {
        let Some(tag) = self.tag.clone() else {
            return Vec::new();
        };
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut ranked: Vec<(usize, u64, String)> = self
            .all_tags
            .iter()
            .filter(|(id, _)| *id != tag.id && !self.blocked.contains(id))
            .filter(|(_, label)| {
                !self
                    .placements_draft
                    .iter()
                    .any(|draft| draft.eq_ignore_ascii_case(label))
            })
            .filter_map(|(id, label)| {
                let lower = label.to_lowercase();
                if lower.starts_with(&query) {
                    return Some((0, *id, label.clone()));
                }
                rank_tag(&query, &lower).map(|score| (score, *id, label.clone()))
            })
            .collect();
        ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)));
        ranked.into_iter().map(|(_, id, label)| (id, label)).collect()
    }

    /// Add `label` to the draft unless it is already there or blocked.
    fn push_placement_label(&mut self, label: String) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let lower = label.to_lowercase();
        if self
            .placements_draft
            .iter()
            .any(|existing| existing.to_lowercase() == lower)
        {
            return;
        }
        let blocked = self
            .all_tags
            .iter()
            .any(|(id, candidate)| (*id == tag.id || self.blocked.contains(id))
                && candidate.to_lowercase() == lower);
        if blocked {
            self.notice = Some("A tag can't be placed under itself or its own child.".to_string());
            return;
        }
        self.placements_draft.push(label);
    }

    /// Swap in a fresh, empty field, restarting the draft editor's typing.
    fn reset_placement_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_placement_input_clear = false;
        self.placement_suggest_cursor = 0;
        self.placement_suggest_active = false;
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Add parent tag…", window, cx);
            state
        });
        let subscription = cx.subscribe(&input, |this, _, event, cx| match event {
            InputEvent::PressEnter { .. } => this.on_placement_input_enter(cx),
            InputEvent::Change => cx.notify(),
            InputEvent::Blur => this.commit_placements(cx),
            _ => {}
        });
        self.placements_input = input.clone();
        self._placements_input_sub = subscription;
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    /// Removing an already-saved parent chip asks first (it un-nests the whole
    /// subtree); removing a staged-only chip just drops the draft entry.
    fn remove_placement_chip(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(label) = self.placements_draft.get(index).cloned() else {
            return;
        };
        let is_saved = self
            .parents
            .iter()
            .any(|parent| parent.label().eq_ignore_ascii_case(&label));
        if !is_saved {
            self.placements_draft.remove(index);
            cx.notify();
            return;
        }
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let child = tag.label();
        let panel = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let panel = panel.clone();
            let (child, label) = (child.clone(), label.clone());
            dialog
                .title(format!("Remove {child} from {label}?"))
                .content(move |content, _window, _cx| {
                    let panel = panel.clone();
                    let (child, label) = (child.clone(), label.clone());
                    content.child(
                        div()
                            .v_flex()
                            .gap_3()
                            .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(format!(
                                "{label} stays where it is; {child} just stops appearing under it."
                            )))
                            .child(
                                div()
                                    .h_flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("tag-settings-keep-placement")
                                            .compact()
                                            .label("Keep it")
                                            .on_click(|_, window, cx| {
                                                window.close_dialog(cx);
                                            }),
                                    )
                                    .child(
                                        Button::new("tag-settings-drop-placement")
                                            .compact()
                                            .label("Remove")
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                if let Err(error) =
                                                    panel.update(cx, |panel, cx| {
                                                        if let Some(pos) = panel
                                                            .placements_draft
                                                            .iter()
                                                            .position(|draft| {
                                                                draft.eq_ignore_ascii_case(&label)
                                                            })
                                                        {
                                                            panel.placements_draft.remove(pos);
                                                            cx.notify();
                                                        }
                                                    })
                                                {
                                                    tracing::error!(
                                                        %error,
                                                        "could not remove the placement"
                                                    );
                                                }
                                            }),
                                    ),
                            ),
                    )
                })
        });
    }

    fn add_dir(&mut self, dir: String, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let mut dirs = self.dirs.clone();
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
        let action = self.store.set_tag_dirs(tag.id, dirs, cx);
        self.run(action, "Directory added.", cx);
    }

    fn remove_dir(&mut self, dir: String, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let dirs = self
            .dirs
            .iter()
            .filter(|existing| **existing != dir)
            .cloned()
            .collect();
        let action = self.store.set_tag_dirs(tag.id, dirs, cx);
        self.run(action, "Directory removed.", cx);
    }

    /// Bind one more repository: its issues land in this tag too (§5.3).
    fn add_repo_binding(&mut self, repo: String, cx: &mut Context<Self>) {
        let (Some(tag), Some(integration_id)) = (self.tag.clone(), self.github_integration_id)
        else {
            return;
        };
        let action = self
            .store
            .bind_tag_repo(tag.id, integration_id, repo.clone(), cx);
        self.run(action, &format!("Syncing with {repo}."), cx);
    }

    /// Unbind one repository: its link goes and the sync target moves to a
    /// remaining repo or clears. Synced tasks keep their local copies.
    fn remove_repo_binding(&mut self, repo: String, cx: &mut Context<Self>) {
        let (Some(tag), Some(integration_id)) = (self.tag.clone(), self.github_integration_id)
        else {
            return;
        };
        let action = self
            .store
            .unbind_tag_repo(tag.id, integration_id, repo.clone(), cx);
        self.run(action, &format!("Unbound {repo}."), cx);
    }

    fn set_isolated_build_cache(&mut self, isolated: bool, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        // Quiet: the switch position plus its label says the state, so no
        // echo at the card bottom.
        let action = self.store.set_tag_isolated_build_cache(tag.id, isolated, cx);
        self.run_quiet(action, cx);
    }

    /// Open or close the repo picker, fetching the account's repositories the
    /// first time it opens.
    fn toggle_repo_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.repo_picker_open = !self.repo_picker_open;
        if self.repo_picker_open {
            self.repo_picker_cursor = 0;
            self.repo_picker_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            self.fetch_repo_candidates(cx);
        }
        cx.notify();
    }

    /// Resolve what the next sync would bind for these directories, so the
    /// section names the detected remote instead of a generic placeholder.
    /// Only runs while GitHub is connected; a previous detection is dropped.
    fn detect_repo(&mut self, cx: &mut Context<Self>) {
        if self.github_integration_id.is_none() || self.dirs.is_empty() {
            self.detected_repo = None;
            self._detect = None;
            return;
        }
        self.detected_repo = None;
        let detect = self.store.detect_github_repo(self.dirs.clone(), cx);
        self._detect = Some(cx.spawn(async move |this, cx| {
            let detected = detect.await.ok().flatten();
            this.update(cx, |this, cx| {
                this.detected_repo = detected;
                this._detect = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn fetch_repo_candidates(&mut self, cx: &mut Context<Self>) {
        if !self.repo_candidates.is_empty() || self.repo_candidates_loading {
            return;
        }
        self.repo_candidates_loading = true;
        self.repo_candidates_error = None;
        let fetch = self.store.github_repos(cx);
        self._repo_fetch = Some(cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |this, cx| {
                this.repo_candidates_loading = false;
                match result {
                    Ok(repos) => {
                        this.repo_candidates = repos
                            .into_iter()
                            .map(|repo| (repo.full_name, repo.private))
                            .collect();
                    }
                    Err(error) => {
                        this.repo_candidates_error =
                            Some(format!("Could not list repositories: {error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The account's repositories matching `query`; an empty query lists them
    /// all, because opening the picker is already the intent.
    fn repo_picker_results(&self, query: &str) -> Vec<(String, bool)> {
        let query = query.trim().to_lowercase();
        let mut matching: Vec<(String, bool)> = self
            .repo_candidates
            .iter()
            .filter(|(full_name, _)| query.is_empty() || full_name.to_lowercase().contains(&query))
            .cloned()
            .collect();
        // Bound repos first, so the current choices stay visible in a long list.
        let bound = self.bound_repos.clone();
        matching.sort_by(|a, b| {
            bound
                .contains(&b.0)
                .cmp(&bound.contains(&a.0))
                .then(a.0.cmp(&b.0))
        });
        matching
    }

    fn on_repo_picker_enter(&mut self, cx: &mut Context<Self>) {
        let query = self.repo_picker_input.read(cx).text().to_string();
        let results = self.repo_picker_results(&query);
        if let Some((full_name, _)) = results.get(self.repo_picker_cursor).cloned() {
            self.repo_picker_open = false;
            self.add_repo_binding(full_name, cx);
        }
    }

    /// The GitHub section: the bound repositories as a list — one row per
    /// repo with a hover cross, then a `+` row to bind another — plus what
    /// the next sync would detect when nothing is bound yet. Rendered only
    /// for directory-backed tags while GitHub is connected: without a local
    /// directory there is nothing to detect from (§5.3, §5.8, §6.4).
    fn github_section(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.github_integration_id?;
        if self.dirs.is_empty() {
            return None;
        }
        let bound = self.bound_repos.clone();
        // What the next sync would bind, shown while the list is empty so an
        // unbound tag with a GitHub remote still names it.
        let detected = match bound.is_empty() {
            true => self.detected_repo.clone(),
            false => None,
        };
        let mut rows: Vec<AnyElement> = bound
            .iter()
            .map(|repo| {
                let value = repo.clone();
                let group = format!("repo-{repo}-group");
                let cross_group = group.clone();
                div()
                    .id(format!("repo-{repo}"))
                    .group(group)
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .hover(|style| style.bg(rgb(0x2a2a2a)))
                    .child(
                        gpui_component::Icon::new(gpui_component_assets::IconName::Github)
                            .with_size(Size::Small),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(repo.clone()),
                    )
                    .child(
                        div()
                            .id(format!("repo-{repo}-remove"))
                            .text_size(px(10.))
                            .text_color(rgb(0x737373))
                            .opacity(0.0)
                            .group_hover(cross_group, |style| style.opacity(1.0))
                            .hover(|style| style.text_color(rgb(0xe5e5e5)))
                            .child("×")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_repo_binding(value.clone(), cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        if let Some(repo) = detected {
            rows.push(
                div()
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .hover(|style| style.bg(rgb(0x2a2a2a)))
                    .child(
                        gpui_component::Icon::new(gpui_component_assets::IconName::Github)
                            .with_size(Size::Small),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(repo),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .child("detected"),
                    )
                    .into_any_element(),
            );
        }
        rows.push(
            Button::new("tag-settings-add-repo")
                .ghost()
                .compact()
                .icon(IconName::Plus)
                .tooltip("Bind another repository")
                .on_click(cx.listener(|this, _, window, cx| this.toggle_repo_picker(window, cx)))
                .into_any_element(),
        );

        Some(
            div()
                .v_flex()
                .gap_1()
                .child(section_label("GitHub repositories"))
                .child(hint(
                    "Tasks in this tag sync with these repositories' issues.",
                ))
                .child(list_box(rows))
                .when_some(self.repo_picker(cx), |this, picker| this.child(picker))
                .into_any_element(),
        )
    }

    /// The build-cache choice: a property of the project rather than of
    /// GitHub, so it shows for any directory-backed tag (§6.4).
    fn isolated_cache_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .h_flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_0p5()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .v_flex()
                    .child(div().text_sm().child("Isolated build cache"))
                    .child(hint(
                        "Each worktree of a run builds into its own target directory instead of \
                         sharing the repository's.",
                    )),
            )
            .child(
                Switch::new("tag-settings-isolated-cache")
                    .checked(self.isolated_build_cache)
                    .tooltip("Build every worktree separately")
                    .on_change(cx.listener(|this, isolated: &bool, _, cx| {
                        this.set_isolated_build_cache(*isolated, cx)
                    })),
            )
            .into_any_element()
    }

    /// The repository list the `Choose…` button opens.
    fn repo_picker(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.repo_picker_open {
            return None;
        }
        let input = self.repo_picker_input.clone();
        let query = input.read(cx).text().to_string();
        let results = self.repo_picker_results(&query);
        let cursor = self.repo_picker_cursor;
        let rows: Vec<AnyElement> = results
            .iter()
            .take(10)
            .enumerate()
            .map(|(index, (full_name, private))| {
                let value = full_name.clone();
                div()
                    .id(("repo-picker-row", index))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .cursor_pointer()
                    .text_sm()
                    .hover(|style| style.bg(rgb(0x2a2a2a)))
                    .when(index == cursor, |style| style.bg(rgb(0x333333)))
                    .child(div().flex_1().min_w_0().truncate().child(value.clone()))
                    .when(*private, |this| {
                        this.child(div().text_xs().text_color(rgb(TEXT_MUTED)).child("private"))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.repo_picker_open = false;
                        this.add_repo_binding(value.clone(), cx);
                    }))
                    .into_any_element()
            })
            .collect();
        Some(
            div()
                .v_flex()
                .gap_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(HAIRLINE))
                .p_1()
                .child(Input::new(&input).appearance(false))
                .when(self.repo_candidates_loading, |this| {
                    this.child(hint("Loading repositories…"))
                })
                .when_some(self.repo_candidates_error.clone(), |this, error| {
                    this.child(hint(&error))
                })
                .when(rows.is_empty() && !self.repo_candidates_loading, |this| {
                    this.child(hint("No repository matches."))
                })
                .child(
                    div()
                        .v_flex()
                        .max_h(px(180.))
                        .overflow_y_scrollbar()
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    fn toggle_dir_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dir_picker_open = !self.dir_picker_open;
        if self.dir_picker_open {
            self.dir_picker_cursor = 0;
            self.reset_dir_picker_input(window, cx);
        }
        cx.notify();
    }

    /// All scanned directories not yet bound to this tag, fuzzy-filtered by
    /// `query`. Empty query returns nothing (no dropdown without intent).
    fn dir_picker_results(&self, query: &str) -> Vec<PathBuf> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, PathBuf)> = self
            .dir_candidates
            .iter()
            .filter(|candidate| {
                let text = candidate.display().to_string();
                !self.dirs.contains(&text)
            })
            .filter_map(|candidate| {
                let text = candidate.display().to_string();
                let lower = text.to_lowercase();
                let score = if lower.starts_with(&query) {
                    0
                } else if lower.contains(&query) {
                    1
                } else {
                    return None;
                };
                Some((score, candidate.clone()))
            })
            .collect();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        scored.into_iter().take(10).map(|(_, p)| p).collect()
    }

    fn on_dir_picker_enter(&mut self, cx: &mut Context<Self>) {
        let results = self.dir_picker_results(
            &self.dir_picker_input.read(cx).text().to_string(),
        );
        if let Some(path) = results.get(self.dir_picker_cursor) {
            let value = path.display().to_string();
            self.dir_picker_open = false;
            self.add_dir(value, cx);
        }
    }

    fn move_dir_picker_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let results = self.dir_picker_results(
            &self.dir_picker_input.read(cx).text().to_string(),
        );
        if results.is_empty() {
            return;
        }
        self.dir_picker_cursor =
            (self.dir_picker_cursor as isize + delta).rem_euclid(results.len() as isize)
                as usize;
        cx.notify();
    }

    fn reset_dir_picker_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("Search directories…", window, cx);
            state
        });
        let sub = cx.subscribe(&input, |this, _, event, cx| match event {
            InputEvent::PressEnter { .. } => this.on_dir_picker_enter(cx),
            InputEvent::Change => {
                this.dir_picker_cursor = 0;
                cx.notify();
            }
            _ => {}
        });
        self.dir_picker_input = input.clone();
        self._dir_picker_sub = sub;
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    }

    fn add_section(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let name = self.section_name.read(cx).text().to_string();
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        self.section_name
            .update(cx, |input, cx| input.set_value("", window, cx));
        let action = self.store.create_section(tag.id, name, cx);
        self.run(action, "Section added.", cx);
    }

    fn remove_section(&mut self, section_id: u64, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let action = self.store.remove_section(tag.id, section_id, cx);
        self.run(action, "Section removed.", cx);
    }

    fn move_section(&mut self, section_id: u64, delta: i64, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let action = self.store.move_section(tag.id, section_id, delta, cx);
        self.run(action, "Sections reordered.", cx);
    }

    fn set_capture(&mut self, app_id: u64, capture: bool, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let action = self.store.set_binding_capture(app_id, tag.id, capture, cx);
        self.run(action, "Capture updated.", cx);
    }

    fn detach_app(&mut self, app_id: u64, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let action = self.store.detach_app_from_tag(app_id, tag.id, cx);
        self.run(action, "App detached.", cx);
    }

    /// The parent-tag editor: chips inside a bordered field with the live
    /// input, plus a deferred suggestion dropdown under the field when the
    /// typed text matches anything.
    fn placements_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.pending_placement_input_clear {
            self.reset_placement_input(window, cx);
        }
        let input = self.placements_input.clone();
        let query = input.read(cx).text().to_string();
        let suggestions = self.placement_suggestions(&query);
        if suggestions.is_empty() {
            self.placement_suggest_cursor = 0;
        } else if self.placement_suggest_cursor >= suggestions.len() {
            self.placement_suggest_cursor = suggestions.len() - 1;
        }
        let suggest_cursor = self.placement_suggest_cursor;

        let chips: Vec<AnyElement> = self
            .placements_draft
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let chip_id = format!("placement-{}", label.trim_start_matches('#'));
                chip(
                    chip_id,
                    label.clone(),
                    None,
                    cx.listener(move |this, _, window, cx| {
                        this.remove_placement_chip(index, window, cx)
                    }),
                )
            })
            .collect();

        let field = div()
            .id("tag-settings-placements-editor")
            .key_context(TAG_EDITOR_CONTEXT)
            .on_action(cx.listener(|this, _: &TagConfirmText, _, cx| {
                this.on_placement_input_enter(cx);
            }))
            .on_action(cx.listener(|this, _: &TagSuggestPrev, _, cx| {
                this.move_placement_suggestion(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &TagSuggestNext, _, cx| {
                this.move_placement_suggestion(1, cx);
            }))
            .flex_1()
            .min_w_0()
            .px_2()
            .py_0p5()
            .rounded_md()
            .border_1()
            .border_color(rgb(HAIRLINE))
            .bg(rgb(APP_BG))
            .h_flex()
            .items_center()
            .gap_1()
            .flex_wrap()
            .children(chips)
            .child(div().flex_1().min_w_0().child(Input::new(&input).appearance(false)));

        let mut root = div().relative().child(field);
        if !suggestions.is_empty() {
            let rows: Vec<AnyElement> = suggestions
                .iter()
                .enumerate()
                .map(|(index, (_, label))| {
                    let label = label.clone();
                    div()
                        .id(("tag-settings-placement-suggest", index))
                        .h_flex()
                        .items_center()
                        .w_full()
                        .px(px(4.))
                        .py(px(1.))
                        .rounded(px(2.))
                        .text_size(px(10.))
                        .text_color(rgb(0xa3a3a3))
                        .cursor_pointer()
                        .when(index == suggest_cursor, |this| this.bg(rgb(0x333333)))
                        .child(format!("#{label}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.push_placement_label(label.clone());
                            this.placement_suggest_active = false;
                            this.pending_placement_input_clear = true;
                            cx.notify();
                        }))
                        .into_any_element()
                })
                .collect();
            root = root.child(deferred(
                div()
                    .absolute()
                    .top(relative(1.))
                    .left(px(0.))
                    .mt(px(4.))
                    .w_full()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(CARD_BG))
                    .border_1()
                    .border_color(rgb(HAIRLINE))
                    .children(rows),
            ));
        }
        root.into_any_element()
    }

    /// The popover card, positioned under the task list header's gear.
    pub fn popover(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.open {
            return div().into_any_element();
        }

        let card = div()
            .id("tag-settings-popover")
            .absolute()
            .top(px(72.))
            .right(px(32.))
            .min_w(px(360.))
            .max_w(px(560.))
            .max_h(px(560.))
            .bg(rgb(CARD_BG))
            .border_1()
            .border_color(rgb(HAIRLINE))
            .rounded_md()
            .px_3()
            .py_2()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)))
            .v_flex()
            .gap_2();

        let Some(tag) = self.tag.clone() else {
            return card.child(section_label("Loading…")).into_any_element();
        };
        let directory_backed = tag.is_project() || !self.dirs.is_empty();

        let heading = div()
            .h_flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .truncate()
                            .child(tag.label()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(rgb(TEXT_MUTED))
                            .child(match (&self.sync_target, directory_backed) {
                                // A bound tag says which repo it syncs with, so
                                // the detected binding is visible instead of a
                                // hidden side effect (spec §5.3).
                                (Some(target), true) => format!(
                                    "Tag settings · project directory · synced with {}",
                                    target.external_id
                                ),
                                (Some(target), false) => {
                                    format!("Tag settings · synced with {}", target.external_id)
                                }
                                (None, true) => "Tag settings · project directory".to_string(),
                                (None, false) => "Tag settings".to_string(),
                            }),
                    ),
            )
            .child(
                Button::new("tag-settings-close")
                    .ghost()
                    .compact()
                    .icon(IconName::Close)
                    .tooltip("Close")
                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
            );

        // -- directories ---------------------------------------------------
        let dir_rows: Vec<AnyElement> = self
            .dirs
            .iter()
            .map(|dir| {
                let value = dir.clone();
                removable_row(
                    format!("dir-{dir}"),
                    dir.clone(),
                    None,
                    cx.listener(move |this, _, _, cx| this.remove_dir(value.clone(), cx)),
                )
            })
            .collect();
        // -- sections ------------------------------------------------------
        let section_count = self.sections.len();
        let section_rows: Vec<AnyElement> = self
            .sections
            .iter()
            .enumerate()
            .map(|(index, section)| {
                let section_id = section.id;
                let name = section.name.clone();
                div()
                    .id(("section", section_id))
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .hover(|s| s.bg(rgb(0x2a2a2a)))
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                    .child(
                        Button::new(("section-up", section_id))
                            .ghost()
                            .compact()
                            .icon(IconName::ArrowUp)
                            .disabled(index == 0)
                            .tooltip("Move up")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.move_section(section_id, -1, cx)
                            })),
                    )
                    .child(
                        Button::new(("section-down", section_id))
                            .ghost()
                            .compact()
                            .icon(IconName::ArrowDown)
                            .disabled(index + 1 == section_count)
                            .tooltip("Move down")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.move_section(section_id, 1, cx)
                            })),
                    )
                    .child(
                        Button::new(("section-remove", section_id))
                            .ghost()
                            .compact()
                            .icon(IconName::Close)
                            .tooltip("Remove section")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_section(section_id, cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();

        // -- apps ----------------------------------------------------------
        let app_rows: Vec<AnyElement> = self
            .bindings
            .iter()
            .map(|(app, binding)| {
                let app_id = app.id;
                let capture = binding.capture_new_tasks;
                let role = match binding.role {
                    BindingRole::FullTag => "owns this tag",
                    BindingRole::Partial => "manages part of it",
                };
                div()
                    .id(("binding", app_id))
                    .h_flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .hover(|s| s.bg(rgb(0x2a2a2a)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .v_flex()
                            .child(div().text_sm().truncate().child(app.label.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(format!("{role} · captures new tasks")),
                            ),
                    )
                    .child(
                        Switch::new(("capture", app_id))
                            .checked(capture)
                            .tooltip("Capture new tasks in this tag")
                            .on_change(cx.listener(move |this, capture: &bool, _, cx| {
                                this.set_capture(app_id, *capture, cx)
                            })),
                    )
                    .child(
                        Button::new(("detach", app_id))
                            .ghost()
                            .compact()
                            .label("Detach")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.detach_app(app_id, cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();

        let isolated_row = self.isolated_cache_row(cx);
        let placements_empty = self.placements_draft.is_empty()
            && self.placements_input.read(cx).text().to_string().trim().is_empty();
        let body = div()
            .id("tag-settings-body")
            .v_flex()
            .gap_3()
            .overflow_y_scrollbar()
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(section_label("Placed under"))
                    .when(placements_empty, |this| {
                        this.child(hint("Not placed under any tag, so it stays at the top level."))
                    })
                    .child(self.placements_editor(window, cx)),
            )
            .child(div().border_t_1().border_color(rgb(HAIRLINE)))
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(section_label("Directories"))
                    .child(hint(
                        "A tag with directories is a project: folder icon, agent pane, and no app \
                         bindings. The first one that exists is the agent's working directory.",
                    ))
                    .child(list_box(dir_rows))
                    .child(
                        div()
                            .relative()
                            .id("tag-settings-dir-picker")
                            .child(
                                Button::new("tag-settings-add-dir")
                                    .ghost()
                                    .compact()
                                    .icon(IconName::Plus)
                                    .tooltip("Add directory")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_dir_picker(window, cx);
                                    })),
                            )
                            .when(self.dir_picker_open, |this| {
                                let input = self.dir_picker_input.clone();
                                let query = input.read(cx).text().to_string();
                                let results = self.dir_picker_results(&query);
                                let cursor = self.dir_picker_cursor;
                                let rows: Vec<AnyElement> = results
                                    .iter()
                                    .enumerate()
                                    .map(|(idx, path)| {
                                        let value = path.display().to_string();
                                        let display = value.clone();
                                        div()
                                            .id(("dir-picker-row", idx))
                                            .h_flex()
                                            .items_center()
                                            .gap_2()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_md()
                                            .cursor_pointer()
                                            .text_sm()
                                            .hover(|s| s.bg(rgb(0x2a2a2a)))
                                            .when(idx == cursor, |s| s.bg(rgb(0x333333)))
                                            .child(div().truncate().child(display))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.dir_picker_open = false;
                                                this.add_dir(value.clone(), cx);
                                            }))
                                            .into_any_element()
                                    })
                                    .collect();
                                this.child(deferred(
                                    div()
                                        .absolute()
                                        .top(px(28.))
                                        .left(px(0.))
                                        .w_full()
                                        .min_w(px(300.))
                                        .max_h(px(200.))
                                        .rounded_md()
                                        .bg(rgb(CARD_BG))
                                        .border_1()
                                        .border_color(rgb(HAIRLINE))
                                        .child(
                                            div()
                                                .id("tag-settings-dir-picker-search")
                                                .key_context(TAG_EDITOR_CONTEXT)
                                                .on_action(cx.listener(
                                                    |this, _: &TagConfirmText, _, cx| {
                                                        this.on_dir_picker_enter(cx);
                                                    },
                                                ))
                                                .on_action(cx.listener(
                                                    |this, _: &TagSuggestPrev, _, cx| {
                                                        this.move_dir_picker_cursor(-1, cx);
                                                    },
                                                ))
                                                .on_action(cx.listener(
                                                    |this, _: &TagSuggestNext, _, cx| {
                                                        this.move_dir_picker_cursor(1, cx);
                                                    },
                                                ))
                                                .p_1()
                                                .border_b_1()
                                                .border_color(rgb(HAIRLINE))
                                                .child(Input::new(&input).appearance(false)),
                                        )
                                        .child(
                                            div()
                                                .v_flex()
                                                .overflow_y_scrollbar()
                                                .children(rows),
                                        ),
                                ))
                            }),
                    ),
            )
            .when(directory_backed, |this| this.child(isolated_row))
            .when_some(self.github_section(cx), |this, section| {
                this.child(div().border_t_1().border_color(rgb(HAIRLINE)))
                    .child(section)
            })
            .child(div().border_t_1().border_color(rgb(HAIRLINE)))
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(section_label("Sections"))
                    .when(self.sections.is_empty(), |this| {
                        this.child(hint("No sections yet."))
                    })
                    .child(div().v_flex().children(section_rows))
                    .child(
                        div()
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(
                                Input::new(&self.section_name).with_size(Size::Small),
                            ))
                            .child(
                                Button::new("tag-settings-add-section")
                                    .compact()
                                    .label("Add")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_section(window, cx)
                                    })),
                            ),
                    ),
            )
            .child(div().border_t_1().border_color(rgb(HAIRLINE)))
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(section_label("Apps"))
                    .when(self.bindings.is_empty(), |this| {
                        this.child(hint("No app is attached to this tag."))
                    })
                    .when(directory_backed, |this| {
                        this.child(hint(
                            "Project directories cannot host an app, so bindings are refused here.",
                        ))
                    })
                    .child(div().v_flex().children(app_rows)),
            );

        let mut card = card.child(heading).child(body);
        if let Some(notice) = self.notice.clone() {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_MUTED))
                    .child(notice),
            );
        }
        card.into_any_element()
    }
}

/// A chip for one placement: label, the path as secondary text, and a cross
/// that only appears on hover (same treatment as the task row's lock glyph).
fn chip(
    id: String,
    label: String,
    path: Option<String>,
    on_remove: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let group = format!("{id}-group");
    let cross_group = group.clone();
    let cross_id = format!("{id}-remove");
    div()
        .id(id)
        .group(group)
        .h_flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_md()
        .border_1()
        .border_color(rgb(HAIRLINE))
        .bg(rgb(0x2a2a2a))
        .child(div().text_sm().child(label))
        .when_some(path, |this, path| {
            this.child(div().text_xs().text_color(rgb(TEXT_MUTED)).child(path))
        })
        .child(
            div()
                .id(cross_id)
                .text_size(px(10.))
                .text_color(rgb(0x737373))
                .opacity(0.0)
                .group_hover(cross_group, |s| s.opacity(1.0))
                .hover(|s| s.text_color(rgb(0xe5e5e5)))
                .child("×")
                .on_click(on_remove),
        )
        .into_any_element()
}

/// A removable list row (directories): label plus a hover-aware cross.
fn removable_row(
    id: String,
    label: String,
    secondary: Option<String>,
    on_remove: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let group = format!("{id}-group");
    let cross_group = group.clone();
    let cross_id = format!("{id}-remove");
    div()
        .id(id)
        .group(group)
        .h_flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_md()
        .hover(|s| s.bg(rgb(0x2a2a2a)))
        .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
        .when_some(secondary, |this, secondary| {
            this.child(div().text_xs().text_color(rgb(TEXT_MUTED)).child(secondary))
        })
        .child(
            div()
                .id(cross_id)
                .text_size(px(10.))
                .text_color(rgb(0x737373))
                .opacity(0.0)
                .group_hover(cross_group, |s| s.opacity(1.0))
                .hover(|s| s.text_color(rgb(0xe5e5e5)))
                .child("×")
                .on_click(on_remove),
        )
        .into_any_element()
}

/// Framed container for the settings lists (directories, repositories):
/// hairline border on a surface slightly darker than the card.
fn list_box(children: Vec<AnyElement>) -> AnyElement {
    div()
        .v_flex()
        .rounded_md()
        .border_1()
        .border_color(rgb(HAIRLINE))
        .bg(rgb(PANEL_BG))
        .p_1()
        .children(children)
        .into_any_element()
}

fn section_label(text: &str) -> AnyElement {    div()
        .text_xs()
        .font_semibold()
        .text_color(rgb(TEXT_MUTED))
        .child(text.to_string())
        .into_any_element()
}

fn hint(text: &str) -> AnyElement {
    div()
        .text_xs()
        .text_color(rgb(TEXT_MUTED))
        .child(text.to_string())
        .into_any_element()
}
