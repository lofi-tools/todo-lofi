use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{button::*, input::*, scroll::ScrollableElement, *};

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
            Styled, Window, div, prelude::FluentBuilder,
        };
        use gpui_component::{ActiveTheme, StyledExt};

        pub struct NavBar {
            pub task_store: Entity<TaskStore>,
            pub selected_tag: Entity<Option<String>>,
        }
        impl Render for NavBar {
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let task_store = self.task_store.read(cx);
                let all_tags = task_store.all_tags().unwrap();
                let selected_tag = self.selected_tag.read(cx).clone();

                div()
                    .w_64()
                    .h_full()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .p_4()
                    .v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(cx.theme().muted_foreground)
                            .mb_2()
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
                    .children(all_tags.iter().map(|tag| {
                        let tag_clone = tag.to_string();
                        let is_selected = Some(tag.clone()) == selected_tag;
                        div()
                            .child(tag.to_owned())
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .hover(|s| s.bg(cx.theme().accent.opacity(0.5)))
                            .when(is_selected, |this| this.bg(cx.theme().accent))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.selected_tag
                                        .update(cx, |tag, _| *tag = Some(tag_clone.clone()));
                                    cx.notify();
                                }),
                            )
                    }))
            }
        }
    }
    pub mod task_list {
        use crate::task_store::TaskStore;
        use gpui::{
            Context, Entity, InteractiveElement, IntoElement, ParentElement, Pixels, Render,
            Styled, Window, div, prelude::FluentBuilder,
        };
        use gpui_component::{
            ActiveTheme, Sizable, StyledExt,
            button::{Button, ButtonVariants},
            input::{Input, InputState},
            v_flex,
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
                if !title.is_empty() {
                    let mut tags = Vec::new();
                    for word in title.split_whitespace() {
                        if word.starts_with('#') && word.len() > 1 {
                            tags.push(word[1..].to_string());
                        }
                    }

                    if let Some(index) = self.editing_index {
                        task_store.insert_task(index, &title);
                        self.editing_index = None;
                        cx.notify();
                    }
                }
            }
            fn toggle_task(&mut self, index: usize, cx: &mut Context<Self>) -> anyhow::Result<()> {
                let task_store = self.task_store.read(cx);
                task_store.toggle_task(index)?;
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
                    //   match event {
                    //     gpui_component::input::InputEvent::PressEnter { .. } => {
                    //         this.save_task(cx);
                    //     }
                    //     _ => {}
                    // }
                })
                .detach();

                let view = cx.entity().clone();
                let selected_tag = self.selected_tag.read(cx);
                let task_store = self.task_store.read(cx);
                let all_tasks = task_store.tasks().unwrap();

                let filtered_tasks = match selected_tag {
                    Some(tag) => all_tasks
                        .iter()
                        .filter(|t| t.tags.contains(&tag))
                        .cloned()
                        .collect::<Vec<_>>(),
                    None => all_tasks,
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
                    .mt_4()
                    .children((0..=filtered_tasks.len()).filter_map(|i| {
                        let task = filtered_tasks.get(i);

                        // If we are filtering, and there is a task at this index,
                        // check if it matches the filter.
                        // The insertion row before the task should also be filtered accordingly?
                        // Actually, if we filter, we might only want to show insertion row at the bottom.
                        // But let's try to keep the "insert before" logic if it matches.

                        let should_show = if let Some(tag) = &selected_tag {
                            task.map(|t| t.tags.contains(tag))
                                .unwrap_or(i == filtered_tasks.len())
                        } else {
                            true
                        };

                        if !should_show {
                            return None;
                        }

                        let view_insert = view.clone();
                        let is_editing = self.editing_index == Some(i);

                        Some(
                            v_flex()
                                .child(
                                    // Insertion point / Input field
                                    div()
                                        .group("plus-row")
                                        .h_flex()
                                        .items_center()
                                        .when(is_editing, |this| this.p_2().gap_4())
                                        .when(!is_editing, |this| this.min_h_2())
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
                                                        .size_4()
                                                        .label("+")
                                                        .on_click(move |_, window, cx| {
                                                            println!("insert-{}", i);
                                                            view_insert.update(cx, |this, cx| {
                                                                this.editing_index = Some(i);
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
                                        ),
                                )
                                .child(
                                    // The task at this index (if exists)
                                    if let Some(task) = task {
                                        let task_id = task.id.clone();
                                        let is_completed = task.completed;
                                        let view_toggle = view.clone();
                                        let task_tags = task.tags.clone();

                                        div()
                                            .h_flex()
                                            .items_center()
                                            .gap_4()
                                            .p_2()
                                            .rounded_md()
                                            .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                                            .child(div().w_8())
                                            .child(
                                                crate::components::checkbox::Checkbox::new(
                                                    format!("check-{}", task_id),
                                                )
                                                .checked(is_completed)
                                                .with_size(Pixels::from(22.))
                                                .on_click(move |_, _, cx| {
                                                    view_toggle.update(cx, |this, cx| {
                                                        this.toggle_task(i, cx);
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
                                            )
                                            .into_any_element()
                                    } else {
                                        div().into_any_element()
                                    },
                                ),
                        )
                    }))
            }
        }
    }
}

pub struct TodoApp {
    task_store: Entity<TaskStore>,
    // editing_index: Option<usize>,
    // input_state: Entity<InputState>,
    selected_tag: Entity<Option<String>>,
    sidebar_ui: Entity<NavBar>,
    task_list_ui: Entity<TaskList>,
}

impl TodoApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input_state = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("New task title...", window, cx);
            state
        });
        let task_store = cx.new(|_| TaskStore::new());
        let selected_tag = cx.new(|_| None);
        let sidebar_ui = cx.new(|_| NavBar {
            task_store: task_store.clone(),
            selected_tag: selected_tag.clone(),
        });
        let task_list_ui = cx.new(|_| TaskList {
            input_state: input_state.clone(),
            selected_tag: selected_tag.clone(),
            task_store: task_store.clone(),
            editing_index: None,
        });

        // cx.subscribe(&input_state, move |this, _, event, cx| {
        //     if let gpui_component::input::InputEvent::PressEnter { .. } = event {
        //         this.save_task(cx);
        //     }
        //     //   match event {
        //     //     gpui_component::input::InputEvent::PressEnter { .. } => {
        //     //         this.save_task(cx);
        //     //     }
        //     //     _ => {}
        //     // }
        // })
        // .detach();

        Self {
            task_store,
            // editing_index: None,
            // input_state,
            selected_tag,
            sidebar_ui,
            task_list_ui,
        }
    }

    // fn save_task(&mut self, cx: &mut Context<Self>) {
    //     let task_store = self.task_store.read(cx);
    //     let title = self
    //         .input_state
    //         .read(cx)
    //         .text()
    //         .to_string()
    //         .trim()
    //         .to_string();
    //     if !title.is_empty() {
    //         let mut tags = Vec::new();
    //         for word in title.split_whitespace() {
    //             if word.starts_with('#') && word.len() > 1 {
    //                 tags.push(word[1..].to_string());
    //             }
    //         }

    //         if let Some(index) = self.editing_index {
    //             task_store.insert_task(index, &title);
    //             self.editing_index = None;
    //             cx.notify();
    //         }
    //     }
    // }

    // fn toggle_task(&mut self, index: usize, cx: &mut Context<Self>) -> anyhow::Result<()> {
    //     let task_store = self.task_store.read(cx);
    //     task_store.toggle_task(index)?;
    //     Ok(())
    // }
}

impl Render for TodoApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().clone();
        // let selected_tag = self.selected_tag.clone();
        let selected_tag = self.selected_tag.read(cx);
        let task_store = self.task_store.read(cx);
        let all_tasks = task_store.tasks().unwrap();

        let mut all_tags = std::collections::BTreeSet::new();
        for task in &all_tasks {
            for tag in &task.tags {
                all_tags.insert(tag.clone());
            }
        }

        let filtered_tasks = match selected_tag {
            Some(tag) => all_tasks
                .iter()
                .filter(|t| t.tags.contains(tag))
                .cloned()
                .collect::<Vec<_>>(),
            None => all_tasks,
        };

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgba(0x1e1e_1eff))
            // .on_action(cx.listener(
            //     |this, _action: &gpui_component::input::Escape, _window, cx| {
            //         this.editing_index = None;
            //         cx.notify();
            //     },
            // ))
            // Sidebar
            .child(self.sidebar_ui.clone())
            // Main Content
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
                            .max_w_128()
                            .mx_auto()
                            .w_full()
                            // Header
                            .child(
                                div()
                                    .v_flex()
                                    .gap_1()
                                    .child(div().text_3xl().font_bold().child(
                                        match selected_tag {
                                            Some(tag) => format!("Tasks: {}", tag),
                                            None => "All Tasks".to_string(),
                                        },
                                    )), // .child(
                                        //     div()
                                        //         .text_sm()
                                        //         .text_color(cx.theme().muted_foreground)
                                        //         .child("Plan your day, one task at a time."),
                                        // ),
                            )
                            // Tasks List Container
                            .child(self.task_list_ui.clone()),
                    ),
            )
    }
}

fn main() {
    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);

    app.run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| TodoApp::new(window, cx));
                // This first level on the window, should be a Root.
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
