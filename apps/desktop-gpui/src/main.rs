use gpui::*;
use gpui_component::{input::*, scroll::ScrollableElement, *};
use std::collections::HashSet;
use storage::prelude::*;

use crate::{
    task_store::TaskStore,
    ui_parts::{navbar::NavBar, task_list::TaskList},
};

pub mod ui_traits;
pub mod components {
    pub mod checkbox;
    pub mod tooltip;
}
pub mod task_store;

pub mod ui_parts {
    pub mod navbar {
        use crate::task_store::TaskStore;
        use gpui::{
            Context, Entity, InteractiveElement, IntoElement, MouseButton, ParentElement, Render,
            Styled, Window, div, prelude::FluentBuilder, px,
        };
        use gpui_component::{ActiveTheme, StyledExt};
        use std::collections::HashSet;

        pub struct NavBar {
            pub task_store: Entity<TaskStore>,
            pub selected_tag: Entity<Option<String>>,
            pub expanded_tags: Entity<HashSet<String>>,
        }

        fn collect_visible_tags(
            task_store: &TaskStore,
            expanded: &HashSet<String>,
        ) -> Vec<(String, usize, bool)> {
            let top_level = task_store.top_level_tags().unwrap_or_default();
            let mut result = Vec::new();

            fn walk(
                tag: &str,
                depth: usize,
                expanded: &HashSet<String>,
                task_store: &TaskStore,
                result: &mut Vec<(String, usize, bool)>,
            ) {
                let children = task_store.children_of(tag).unwrap_or_default();
                let has_children = !children.is_empty();
                result.push((tag.to_string(), depth, has_children));

                if expanded.contains(tag) {
                    for child in &children {
                        walk(child, depth + 1, expanded, task_store, result);
                    }
                }
            }

            for tag in &top_level {
                walk(tag, 0, expanded, task_store, &mut result);
            }

            result
        }

        impl Render for NavBar {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let expanded = self.expanded_tags.read(cx).clone();
                let selected_tag = self.selected_tag.read(cx).clone();

                let visible_tags = {
                    let task_store = self.task_store.read(cx);
                    collect_visible_tags(task_store, &expanded)
                };

                div()
                    .w_64()
                    .h_full()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .p_2()
                    .v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .mb_1()
                            .child("Tags"),
                    )
                    .child(
                        div()
                            .child("All Tasks")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
                            .when(selected_tag.is_none(), |this| this.bg(cx.theme().accent))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.selected_tag.update(cx, |tag, _| *tag = None);
                                    cx.notify();
                                }),
                            ),
                    )
                    .children(
                        visible_tags
                            .into_iter()
                            .map(|(tag_name, depth, has_children)| {
                                let is_expanded = expanded.contains(&tag_name);
                                let is_selected = Some(tag_name.clone()) == selected_tag;
                                let indent = depth;
                                let tag_for_toggle = tag_name.clone();
                                let tag_for_select = tag_name.clone();

                                let disclosure = if has_children {
                                    let indicator = if is_expanded { "▼" } else { "▶" };
                                    div()
                                        .w_5()
                                        .flex_none()
                                        .h_flex()
                                        .justify_center()
                                        .items_center()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(indicator)
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                let tag = tag_for_toggle.clone();
                                                this.expanded_tags.update(cx, move |tags, _| {
                                                    if tags.contains(&tag) {
                                                        tags.remove(&tag);
                                                    } else {
                                                        tags.insert(tag);
                                                    }
                                                });
                                                cx.notify();
                                            }),
                                        )
                                } else {
                                    div().w_5().flex_none()
                                };

                                div()
                                    .h_flex()
                                    .items_center()
                                    .ml(px(indent as f32 * 8.0))
                                    .child(disclosure)
                                    .child(
                                        div()
                                            .flex_1()
                                            .child(tag_name)
                                            .px_2()
                                            .py_0p5()
                                            .rounded_md()
                                            .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
                                            .when(is_selected, |this| this.bg(cx.theme().accent))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    this.selected_tag.update(cx, |tag, _| {
                                                        *tag = Some(tag_for_select.clone())
                                                    });
                                                    cx.notify();
                                                }),
                                            ),
                                    )
                            }),
                    )
            }
        }
    }
    pub mod task_list {
        use crate::task_store::TaskStore;
        use gpui::{
            Context, Entity, InteractiveElement, IntoElement, ParentElement, Pixels, Render,
            Styled, Window, div, prelude::FluentBuilder, px,
        };
        use gpui_component::{
            ActiveTheme, Sizable, StyledExt,
            button::{Button, ButtonVariants},
            input::{Input, InputState},
        };

        pub struct TaskList {
            pub selected_tag: Entity<Option<String>>,
            pub task_store: Entity<TaskStore>,
            pub editing_index: Option<usize>,
            pub input_state: Entity<InputState>,
        }
        impl TaskList {
            fn save_task(&mut self, cx: &mut Context<Self>) {
                let task_store = self.task_store.read(cx);
                let title = self
                    .input_state
                    .read(cx)
                    .text()
                    .to_string()
                    .trim()
                    .to_string();
                if !title.is_empty()
                    && let Some(index) = self.editing_index
                {
                    let _ = task_store.insert_task(index, &title);
                    self.editing_index = None;
                    cx.notify();
                }
            }
            fn toggle_task(&mut self, id: u64, cx: &mut Context<Self>) -> anyhow::Result<()> {
                let task_store = self.task_store.read(cx);
                task_store.toggle_task(id)?;
                Ok(())
            }
        }
        impl Render for TaskList {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                // Subscribe to input events
                cx.subscribe(&self.input_state, move |this, _, event, cx| {
                    if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                        this.save_task(cx);
                    }
                })
                .detach();

                let view = cx.entity().clone();
                let selected_tag = self.selected_tag.read(cx);
                let task_store = self.task_store.read(cx);

                let filtered_tasks = match selected_tag.as_deref() {
                    Some(tag) => task_store.tasks_for_tag(tag).unwrap_or_default(),
                    None => task_store.tasks().unwrap_or_default(),
                };

                div()
                    .on_action(cx.listener(
                        |this, _action: &gpui_component::input::Escape, _window, cx| {
                            this.editing_index = None;
                            cx.notify();
                        },
                    ))
                    .v_flex()
                    .relative()
                    .child({
                        let is_editing = self.editing_index == Some(0);
                        let view_insert = view.clone();
                        div()
                            .group("plus-row")
                            .h_flex()
                            .items_center()
                            .relative()
                            .when(is_editing, |this| this.gap_4())
                            .when(!is_editing, |this| {
                                this.h(px(12.))
                                    .mt(-px(6.))
                                    .mb(-px(6.))
                            })
                            .child(
                                div()
                                    .w_8()
                                    .h_flex()
                                    .justify_center()
                                    .when(!is_editing, |this| {
                                        this.opacity(0.0)
                                            .group_hover("plus-row", |s| s.opacity(1.0))
                                    })
                                    .child(
                                        Button::new("insert-0")
                                            .ghost()
                                            .p_0()
                                            .size_6()
                                            .label("+")
                                            .on_click(move |_, window, cx| {
                                                view_insert.update(cx, |this, cx| {
                                                    this.editing_index = Some(0);
                                                    this.input_state.update(
                                                        cx,
                                                        |state, cx| {
                                                            state.set_value("", window, cx);
                                                            state.focus(window, cx);
                                                        },
                                                    );
                                                    cx.notify();
                                                });
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .when(is_editing, |this| {
                                        this.child(
                                            Input::new(&self.input_state)
                                                .cleanable(true),
                                        )
                                    })
                                    .when(!is_editing, |this| {
                                        this.opacity(0.0)
                                            .group_hover("plus-row", |s| s.opacity(1.0))
                                            .h_px()
                                            .bg(cx.theme().border.opacity(0.3))
                                    }),
                            )
                    })
                    .children((0..=filtered_tasks.len()).flat_map(|i| {
                        let task = filtered_tasks.get(i);

                        let should_show = task.is_some();

                        if !should_show {
                            return vec![].into_iter();
                        }

                        let view_insert = view.clone();
                        let insert_at = i + 1;
                        let is_editing = self.editing_index == Some(insert_at);

                        let plus_row = div()
                            .group("plus-row")
                            .h_flex()
                            .items_center()
                            .relative()
                            .when(is_editing, |this| this.gap_4())
                            .when(!is_editing, |this| {
                                this.h(px(12.))
                                    .mt(-px(6.))
                                    .mb(-px(6.))
                            })
                            .child(
                                div()
                                    .w_8()
                                    .h_flex()
                                    .justify_center()
                                    .when(!is_editing, |this| {
                                        this.opacity(0.0)
                                            .group_hover("plus-row", |s| s.opacity(1.0))
                                    })
                                    .child(
                                        Button::new(format!("insert-{}", i))
                                            .ghost()
                                            .p_0()
                                            .size_6()
                                            .label("+")
                                            .on_click(move |_, window, cx| {
                                                println!("insert-{}", insert_at);
                                                view_insert.update(cx, |this, cx| {
                                                    this.editing_index = Some(insert_at);
                                                    this.input_state.update(
                                                        cx,
                                                        |state, cx| {
                                                            state.set_value(
                                                                "", window, cx,
                                                            );
                                                            state.focus(window, cx);
                                                        },
                                                    );
                                                    cx.notify();
                                                });
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .when(is_editing, |this| {
                                        this.child(
                                            Input::new(&self.input_state)
                                                .cleanable(true),
                                        )
                                    })
                                    .when(!is_editing, |this| {
                                        this.opacity(0.0)
                                            .group_hover("plus-row", |s| s.opacity(1.0))
                                            .h_px()
                                            .bg(cx.theme().border.opacity(0.3))
                                    }),
                            );

                        let mut elements: Vec<_> = Vec::new();

                        if let Some(task) = task {
                            let task_id = task.id;
                            let is_completed = task.completed;
                            let view_toggle = view.clone();
                            let task_tags = task.tags.clone();

                            let task_item = div()
                                .h_flex()
                                .items_center()
                                .gap_4()
                                .py_0p5()
                                .pl_2()
                                .ml(px(8.))
                                .rounded_md()
                                .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                                .when(is_completed, |this| this.opacity(0.4))
                                .child(
                                    crate::components::checkbox::Checkbox::new(
                                        format!("check-{}", task_id),
                                    )
                                    .checked(is_completed)
                                    .with_size(Pixels::from(22.))
                                    .on_click(move |_, _, cx| {
                                        let task_id = task_id;
                                        view_toggle.update(cx, |this, cx| {
                                            let _ = this.toggle_task(task_id, cx);
                                            cx.notify();
                                        });
                                    }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .v_flex()
                                        .child(
                                            div()
                                                .text_base()
                                                .when(is_completed, |this| {
                                                    this.text_color(
                                                        cx.theme().muted_foreground,
                                                    )
                                                })
                                                .child(task.title.clone()),
                                        )
                                        .child(div().h_flex().gap_2().children(
                                            task_tags.into_iter().map(|t| {
                                                div()
                                                    .text_xs()
                                                    .px_1()
                                                    .rounded_sm()
                                                    .bg(cx.theme().muted)
                                                    .text_color(
                                                        cx.theme().muted_foreground,
                                                    )
                                                    .child(format!("#{}", t))
                                            }),
                                        )),
                                );

                            elements.push(task_item.into_any_element());
                        }

                        elements.push(plus_row.into_any_element());

                        elements.into_iter()
                    }))
            }
        }
    }
}

pub struct TodoApp {
    task_store: Entity<TaskStore>,
    selected_tag: Entity<Option<String>>,
    sidebar_ui: Entity<NavBar>,
    task_list_ui: Entity<TaskList>,
}

impl TodoApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, task_store: Entity<TaskStore>) -> Self {
        let input_state = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("New task title...", window, cx);
            state
        });
        let selected_tag = cx.new(|_| None);
        let expanded_tags = cx.new(|_| HashSet::new());
        let sidebar_ui = cx.new(|_| NavBar {
            task_store: task_store.clone(),
            selected_tag: selected_tag.clone(),
            expanded_tags,
        });
        let task_list_ui = cx.new(|_| TaskList {
            input_state: input_state.clone(),
            selected_tag: selected_tag.clone(),
            task_store: task_store.clone(),
            editing_index: None,
        });

        Self {
            task_store,
            selected_tag,
            sidebar_ui,
            task_list_ui,
        }
    }
}

impl Render for TodoApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _view = cx.entity().clone();
        let selected_tag = self.selected_tag.read(cx);
        let task_store = self.task_store.read(cx);
        let _all_tasks = task_store.tasks().unwrap();

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgba(0x1e1e_1eff))
            .child(self.sidebar_ui.clone())
            .child(
                div()
                    .flex_1()
                    .v_flex()
                    .p_8()
                    .gap_6()
                    .overflow_y_scrollbar()
                    .child(
                        div()
                            .v_flex()
                            .gap_1()
                            .w_full()
                            .child(div().v_flex().gap_1().child(
                                div().text_3xl().font_bold().ml(px(16.)).mb(px(16.)).child(
                                    match selected_tag {
                                        Some(tag) => format!("Tasks: {}", tag),
                                        None => "All Tasks".to_string(),
                                    },
                                ),
                            ))
                            .child(self.task_list_ui.clone()),
                    ),
            )
    }
}

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Failed to create Tokio runtime");

    let task_store_data = rt.block_on(async {
        let config = StorageConfig {
            db_uri: "turso::memory:".to_string(),
        };
        let mut store = TodoStore::new(&config).await.unwrap();
        TaskStore::load_from_storage(&mut store).await.unwrap()
    });

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);

    app.run(move |cx| {
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);

        let task_store_entity = cx.new(|_| task_store_data);

        cx.open_window(WindowOptions::default(), |window, cx| {
            let view = cx.new(|cx| TodoApp::new(window, cx, task_store_entity.clone()));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("Failed to open window");
    });
}
