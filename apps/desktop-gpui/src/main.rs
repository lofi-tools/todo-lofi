use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{button::*, checkbox::*, input::*, *};

struct Task {
    id: String,
    title: String,
    completed: bool,
}

pub struct TodoApp {
    tasks: Vec<Task>,
    editing_index: Option<usize>,
    input_state: Entity<InputState>,
}

impl TodoApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input_state = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("New task title...", window, cx);
            state
        });

        // Subscribe to input events
        cx.subscribe(&input_state, move |this, _, event, cx| match event {
            gpui_component::input::InputEvent::PressEnter { .. } => {
                this.save_task(cx);
            }
            _ => {}
        })
        .detach();

        Self {
            tasks: vec![
                Task {
                    id: "1".into(),
                    title: "Learn GPUI".into(),
                    completed: false,
                },
                Task {
                    id: "2".into(),
                    title: "Build a todo app".into(),
                    completed: true,
                },
                Task {
                    id: "3".into(),
                    title: "Explore gpui-component".into(),
                    completed: false,
                },
            ],
            editing_index: None,
            input_state,
        }
    }

    fn save_task(&mut self, cx: &mut Context<Self>) {
        let title = self
            .input_state
            .read(cx)
            .text()
            .to_string()
            .trim()
            .to_string();
        if !title.is_empty() {
            if let Some(index) = self.editing_index {
                self.tasks.insert(
                    index,
                    Task {
                        id: uuid::Uuid::new_v4().to_string(),
                        title,
                        completed: false,
                    },
                );
                self.editing_index = None;
                cx.notify();
            }
        }
    }

    fn toggle_task(&mut self, index: usize) {
        if let Some(task) = self.tasks.get_mut(index) {
            task.completed = !task.completed;
        }
    }
}

impl Render for TodoApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().clone();

        div()
            .v_flex()
            .size_full()
            .bg(cx.theme().background)
            .on_action(cx.listener(
                |this, _action: &gpui_component::input::Escape, _window, cx| {
                    this.editing_index = None;
                    cx.notify();
                },
            ))
            .child(
                div()
                    .v_flex()
                    .p_8()
                    .gap_6()
                    .max_w_128()
                    .mx_auto()
                    // Header
                    .child(
                        div()
                            .v_flex()
                            .gap_1()
                            .child(div().text_3xl().font_bold().child("Tasks"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Plan your day, one task at a time."),
                            ),
                    )
                    // Tasks List Container
                    .child(
                        div()
                            .v_flex()
                            .relative()
                            .children((0..=self.tasks.len()).map(|i| {
                                let view_insert = view.clone();
                                let is_editing = self.editing_index == Some(i);

                                v_flex()
                                    .child(
                                        // Insertion point / Input field
                                        div()
                                            .h_flex()
                                            .items_center()
                                            .min_h_6()
                                            .child(
                                                div().w_8().h_flex().justify_center().child(
                                                    Button::new(format!("insert-{}", i))
                                                        .ghost()
                                                        .p_0()
                                                        .size_5()
                                                        .label("+")
                                                        .on_click(move |_, window, cx| {
                                                            _ = view_insert.update(
                                                                cx,
                                                                |this, cx| {
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
                                                                },
                                                            );
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
                                                        this.h_px()
                                                            .bg(cx.theme().border.opacity(0.3))
                                                    }),
                                            ),
                                    )
                                    .child(
                                        // The task at this index (if exists)
                                        if let Some(task) = self.tasks.get(i) {
                                            let task_id = task.id.clone();
                                            let is_completed = task.completed;
                                            let view_toggle = view.clone();

                                            div()
                                                .h_flex()
                                                .items_center()
                                                .gap_4()
                                                .p_2()
                                                .rounded_md()
                                                .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                                                .child(div().w_8())
                                                .child(
                                                    Checkbox::new(format!("check-{}", task_id))
                                                        .checked(is_completed)
                                                        .on_click(move |_, _, cx| {
                                                            _ = view_toggle.update(
                                                                cx,
                                                                |this, cx| {
                                                                    this.toggle_task(i);
                                                                    cx.notify();
                                                                },
                                                            );
                                                        }),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .text_base()
                                                        .when(is_completed, |this| {
                                                            this.text_color(
                                                                cx.theme().muted_foreground,
                                                            )
                                                        })
                                                        .child(task.title.clone()),
                                                )
                                                .into_any_element()
                                        } else {
                                            div().into_any_element()
                                        },
                                    )
                            })),
                    ),
            )
    }
}

fn main() {
    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);

    app.run(move |cx| {
        // This must be called before using any GPUI Component features.
        gpui_component::init(cx);

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
