use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{button::*, checkbox::*, *};

struct Task {
    id: String,
    title: String,
    completed: bool,
}

pub struct TodoApp {
    tasks: Vec<Task>,
}

impl TodoApp {
    pub fn new() -> Self {
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
        }
    }

    fn toggle_task(&mut self, index: usize) {
        if let Some(task) = self.tasks.get_mut(index) {
            task.completed = !task.completed;
        }
    }

    fn insert_task(&mut self, index: usize) {
        let new_task = Task {
            id: uuid::Uuid::new_v4().to_string(),
            title: format!("New Task at {}", index),
            completed: false,
        };
        self.tasks.insert(index, new_task);
    }
}

impl Render for TodoApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().clone();

        div().v_flex().size_full().bg(cx.theme().background).child(
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
                        .children(self.tasks.iter().enumerate().map(|(i, task)| {
                            let task_id = task.id.clone();
                            let is_completed = task.completed;
                            let view_toggle = view.clone();
                            let view_insert = view.clone();

                            div()
                                .v_flex()
                                .child(
                                    // Insertion point above
                                    div()
                                        .h_flex()
                                        .items_center()
                                        .h_6()
                                        .child(
                                            div().w_8().h_flex().justify_center().child(
                                                Button::new(format!("insert-{}", i))
                                                    .ghost()
                                                    .p_0()
                                                    .size_5()
                                                    .label("+")
                                                    .on_click(move |_, _, cx| {
                                                        _ = view_insert.update(cx, |this, cx| {
                                                            this.insert_task(i);
                                                            cx.notify();
                                                        });
                                                    }),
                                            ),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .h_px()
                                                .bg(cx.theme().border.opacity(0.3)),
                                        ),
                                )
                                .child(
                                    div()
                                        .h_flex()
                                        .items_center()
                                        .gap_4()
                                        .p_2()
                                        .rounded_md()
                                        .hover(|s| s.bg(cx.theme().muted.opacity(0.5)))
                                        .child(
                                            div().w_8().h_flex().justify_center(), // Empty space for alignment with the + button above
                                        )
                                        .child(
                                            Checkbox::new(format!("check-{}", task_id))
                                                .checked(is_completed)
                                                .on_click(move |_, _, cx| {
                                                    _ = view_toggle.update(cx, |this, cx| {
                                                        this.toggle_task(i);
                                                        cx.notify();
                                                    });
                                                }),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .text_base()
                                                .when(is_completed, |this| {
                                                    this.text_color(cx.theme().muted_foreground)
                                                })
                                                .child(task.title.clone()),
                                        ),
                                )
                        }))
                        // Final insertion point
                        .child(
                            div()
                                .h_flex()
                                .items_center()
                                .h_6()
                                .child(div().w_8().h_flex().justify_center().child({
                                    let view_insert = view.clone();
                                    let final_index = self.tasks.len();
                                    Button::new("insert-final")
                                        .ghost()
                                        .p_0()
                                        .size_5()
                                        .label("+")
                                        .on_click(move |_, _, cx| {
                                            _ = view_insert.update(cx, |this, cx| {
                                                this.insert_task(final_index);
                                                cx.notify();
                                            });
                                        })
                                }))
                                .child(div().flex_1().h_px().bg(cx.theme().border.opacity(0.3))),
                        ),
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
                let view = cx.new(|_| TodoApp::new());
                // This first level on the window, should be a Root.
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
