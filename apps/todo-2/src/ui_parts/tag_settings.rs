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
    ParentElement, StatefulInteractiveElement, Styled, Task, Window, div, prelude::FluentBuilder, px,
    rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement;
use gpui_component::switch::Switch;
use gpui_component::{Disableable, IconName, Size, Sizable, StyledExt, WindowExt};
use std::collections::HashSet;
use std::path::PathBuf;
use storage::prelude::*;

use crate::store::Store;
use crate::theme::{CARD_BG, HAIRLINE, TEXT_MUTED};

/// How many picker rows render at once; the lists scroll past this.
const PICKER_ROWS: usize = 40;

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
    /// Tags this tag is placed under (one chip each).
    parents: Vec<Tag>,
    dirs: Vec<String>,
    sections: Vec<TagSection>,
    bindings: Vec<(App, AppTagBinding)>,
    /// Every tag with the first path it is reachable through: the placement
    /// picker's list and its secondary path text.
    catalogue: Vec<(Tag, String)>,
    /// Tags that may not be chosen as a parent: this tag and its descendants
    /// (the store would reject a cycle).
    blocked: HashSet<u64>,
    /// Directories offered for this tag, from the home-directory scan.
    dir_candidates: Vec<PathBuf>,
    section_name: Entity<InputState>,
    /// One-line result of the last action, so writes are visible.
    notice: Option<String>,
    _fetch: Option<Task<()>>,
}

impl EventEmitter<TagSettingsEvent> for TagSettingsPanel {}

impl TagSettingsPanel {
    pub fn new(store: Store, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            store,
            open: false,
            tag: None,
            parents: Vec::new(),
            dirs: Vec::new(),
            sections: Vec::new(),
            bindings: Vec::new(),
            catalogue: Vec::new(),
            blocked: HashSet::new(),
            dir_candidates: Vec::new(),
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
        let tree = store.tag_tree_rows(cx);
        let parents = store.tag_parents(tag_id, cx);
        let dirs = store.tag_dirs(tag_id, cx);
        let sections = store.tag_sections(tag_id, cx);
        let bindings = store.bindings_for_tag(tag_id, cx);
        let descendants = store.tag_descendant_ids(tag_id, cx);
        self._fetch = Some(cx.spawn(async move |this, cx| {
            // One tree call gives both the picker's tag list and each tag's
            // path label; a tag placed under several parents keeps the first
            // path it is reached through.
            let mut catalogue: Vec<(Tag, String)> = Vec::new();
            let mut seen: HashSet<u64> = HashSet::new();
            for row in tree.await.unwrap_or_default() {
                if seen.insert(row.tag.id) {
                    catalogue.push((row.tag, row.path.join(" › ")));
                }
            }
            let parents = parents.await.unwrap_or_default();
            let dirs = dirs.await.unwrap_or_default();
            let sections = sections.await.unwrap_or_default();
            let bindings = bindings.await.unwrap_or_default();
            let blocked = descendants.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.catalogue = catalogue;
                this.parents = parents;
                this.dirs = dirs;
                this.sections = sections;
                this.bindings = bindings;
                this.blocked = blocked.into_iter().collect();
                this._fetch = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// The first path a tag is reachable through, for a chip's secondary text.
    fn path_of(&self, tag_id: u64) -> Option<String> {
        self.catalogue
            .iter()
            .find(|(tag, _)| tag.id == tag_id)
            .map(|(_, path)| path.clone())
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

    fn place_under(&mut self, parent_id: u64, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let action = self.store.place_tag_under(tag.id, parent_id, cx);
        self.run(action, "Placed under the tag.", cx);
    }

    fn unplace(&mut self, parent_id: u64, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let action = self.store.unplace_tag_from(tag.id, parent_id, cx);
        self.run(action, "Removed from that tag.", cx);
    }

    /// Removing a placement is the one action that asks first: it moves the
    /// whole subtree out of the nav, which is not obvious from one chip.
    fn confirm_unplace(&mut self, parent_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tag) = self.tag.clone() else {
            return;
        };
        let child = tag.label();
        let parent = self
            .parents
            .iter()
            .find(|parent| parent.id == parent_id)
            .map(|parent| parent.label())
            .unwrap_or_else(|| "that tag".to_string());
        let panel = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            let panel = panel.clone();
            let (child, parent) = (child.clone(), parent.clone());
            dialog
                .title(format!("Remove {child} from {parent}?"))
                .content(move |content, _window, _cx| {
                    let panel = panel.clone();
                    let (child, parent) = (child.clone(), parent.clone());
                    content.child(
                        div()
                            .v_flex()
                            .gap_3()
                            .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(format!(
                                "{parent} stays where it is; {child} just stops appearing under it. \
                                 Any other tag it is placed under is untouched."
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
                                                if let Err(error) = panel.update(cx, |panel, cx| {
                                                    panel.unplace(parent_id, cx)
                                                }) {
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

    /// The popover card, positioned under the task list header's gear.
    pub fn popover(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.open {
            return div().into_any_element();
        }

        let card = div()
            .id("tag-settings-popover")
            .absolute()
            .top(px(72.))
            .right(px(32.))
            .w(px(360.))
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
        let tag_id = tag.id;
        let directory_backed = tag.is_project() || !self.dirs.is_empty();

        let heading = div()
            .h_flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .v_flex()
                    .child(div().text_sm().font_semibold().child(tag.label()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(TEXT_MUTED))
                            .child(if directory_backed {
                                "Tag settings · project directory"
                            } else {
                                "Tag settings"
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

        // -- placed under --------------------------------------------------
        let placements: Vec<AnyElement> = self
            .parents
            .iter()
            .map(|parent| {
                let parent_id = parent.id;
                let path = self.path_of(parent_id);
                chip(
                    format!("placement-{parent_id}"),
                    parent.label(),
                    path,
                    cx.listener(move |this, _, window, cx| {
                        this.confirm_unplace(parent_id, window, cx)
                    }),
                )
            })
            .collect();

        let eligible = eligible_parents(&self.catalogue, tag_id, &self.blocked, &self.parents);
        let placement_candidates: Vec<AnyElement> = eligible
            .iter()
            .take(PICKER_ROWS)
            .map(|(id, label, path)| {
                let parent_id = *id;
                picker_row(
                    format!("place-under-{parent_id}"),
                    label.clone(),
                    Some(path.clone()),
                    cx.listener(move |this, _, _, cx| this.place_under(parent_id, cx)),
                )
            })
            .collect();

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
        let dir_choices: Vec<AnyElement> = self
            .dir_candidates
            .iter()
            .filter(|candidate| {
                let text = candidate.display().to_string();
                !self.dirs.contains(&text)
            })
            .take(PICKER_ROWS)
            .map(|candidate| {
                let value = candidate.display().to_string();
                picker_row(
                    format!("add-dir-{value}"),
                    value.clone(),
                    None,
                    cx.listener(move |this, _, _, cx| this.add_dir(value.clone(), cx)),
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
                    .when(self.parents.is_empty(), |this| {
                        this.child(hint("Not placed under any tag, so it stays at the top level."))
                    })
                    .child(div().h_flex().flex_wrap().gap_1().children(placements)),
            )
            .child(
                div()
                    .v_flex()
                    .gap_1()
                    .child(section_label("Add to a tag"))
                    .when(placement_candidates.is_empty(), |this| {
                        this.child(hint("No tag can take it: the rest are its own descendants."))
                    })
                    .child(div().v_flex().children(placement_candidates)),
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
                    .child(div().v_flex().children(dir_rows))
                    .when(dir_choices.is_empty(), |this| {
                        this.child(hint("No other directory from the project scan fits here."))
                    })
                    .child(div().v_flex().children(dir_choices)),
            )
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

/// A clickable picker row: flat label with the path as secondary text.
fn picker_row(
    id: String,
    label: String,
    path: Option<String>,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h_flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(rgb(0x2a2a2a)))
        .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
        .when_some(path, |this, path| {
            this.child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_MUTED))
                    .child(path),
            )
        })
        .on_click(on_click)
        .into_any_element()
}

fn section_label(text: &str) -> AnyElement {
    div()
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

/// The tags that may become `tag_id`'s parent: not the tag itself, not one of
/// its descendants (the store refuses a cycle), and not one it is already
/// placed under — the picker only ever adds, and removal is the chip's cross.
/// Sorted by label so the list reads predictably.
fn eligible_parents(
    catalogue: &[(Tag, String)],
    tag_id: u64,
    blocked: &HashSet<u64>,
    parents: &[Tag],
) -> Vec<(u64, String, String)> {
    let mut eligible: Vec<(u64, String, String)> = catalogue
        .iter()
        .filter(|(candidate, _)| {
            candidate.id != tag_id
                && !blocked.contains(&candidate.id)
                && !parents.iter().any(|parent| parent.id == candidate.id)
        })
        .map(|(candidate, path)| {
            (
                candidate.id,
                candidate.label(),
                path.clone(),
            )
        })
        .collect();
    eligible.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    eligible
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(id: u64, name: &str, display: Option<&str>) -> Tag {
        Tag {
            id,
            name: name.to_string(),
            display_name: display.map(|display| display.to_string()),
        }
    }

    fn catalogue(entries: &[(Tag, &str)]) -> Vec<(Tag, String)> {
        entries
            .iter()
            .map(|(tag, path)| (tag.clone(), path.to_string()))
            .collect()
    }

    /// The picker leaves out exactly what the store would reject, so an
    /// invalid choice is unreachable rather than an error after the click.
    #[test]
    fn eligible_parents_excludes_self_descendants_and_existing_placements() {
        let work = tag(1, "Work", None);
        let home = tag(2, "Home", None);
        let child = tag(3, "Child", None);
        let project = tag(4, "project:/tmp/x", Some("x"));
        let entries = catalogue(&[
            (work.clone(), "Work"),
            (home.clone(), "Home"),
            (child.clone(), "Work › Child"),
            (project.clone(), "x"),
        ]);
        let blocked: HashSet<u64> = [child.id].into_iter().collect();
        let parents = vec![home];

        let eligible = eligible_parents(&entries, project.id, &blocked, &parents);
        assert_eq!(
            eligible
                .iter()
                .map(|(id, _, _)| *id)
                .collect::<Vec<_>>(),
            vec![work.id]
        );
        assert_eq!(eligible[0].1, "Work");
        assert_eq!(eligible[0].2, "Work");
    }

    #[test]
    fn eligible_parents_sorts_by_label_case_insensitively() {
        let beta = tag(1, "beta", None);
        let alpha = tag(2, "Alpha", None);
        let entries = catalogue(&[(beta, "beta"), (alpha, "Alpha")]);

        let eligible = eligible_parents(&entries, 99, &HashSet::new(), &[]);
        assert_eq!(
            eligible
                .iter()
                .map(|(_, label, _)| label.clone())
                .collect::<Vec<_>>(),
            vec!["Alpha".to_string(), "beta".to_string()]
        );
    }

    /// A project's row shows the directory name, not the opaque
    /// `project:{path}` tag name.
    #[test]
    fn eligible_parents_prefers_the_display_label() {
        let project = tag(4, "project:/tmp/x", Some("x"));
        let entries = catalogue(&[(project, "x")]);

        let eligible = eligible_parents(&entries, 99, &HashSet::new(), &[]);
        assert_eq!(eligible[0].1, "x");
    }
}
