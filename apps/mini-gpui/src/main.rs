use gpui::*;
use gpui_component::input::*;
use gpui_component::{StyledExt, Theme, ThemeMode};
use std::sync::{Arc, RwLock};
use storage::prelude::*;

struct MiniTodo {
    tasks: Arc<RwLock<Vec<storage::Task>>>,
    input: Entity<InputState>,
    needs_clear: bool,
    _subscription: Subscription,
}

impl Render for MiniTodo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_clear {
            self.needs_clear = false;
            self.input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }

        let tasks = self.tasks.read().unwrap().clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1a1a1a))
            .p_8()
            .gap_4()
            .child(
                div()
                    .text_2xl()
                    .font_bold()
                    .text_color(rgb(0xe5e5e5ff))
                    .child("Mini Todo"),
            )
            .child(Input::new(&self.input))
            .child(
                div()
                    .flex_1()
                    .v_flex()
                    .gap_2()
                    .children(tasks.into_iter().map(|task| {
                        div()
                            .h_flex()
                            .gap_3()
                            .py_1()
                            .px_3()
                            .rounded_md()
                            .hover(|s| s.bg(rgb(0x2a2a2aff)))
                            .child(
                                div()
                                    .text_base()
                                    .text_color(rgb(0xa3a3a3ff))
                                    .child(task.title.clone()),
                            )
                    })),
            )
    }
}

impl MiniTodo {
    fn new(
        input: Entity<InputState>,
        store: Entity<Option<Arc<tokio::sync::Mutex<TodoStore>>>>,
        tasks: Arc<RwLock<Vec<storage::Task>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let entity = cx.entity();
        let tasks_clone = tasks.clone();
        let input_clone = input.clone();
        let store_clone = store.clone();

        let subscription = cx.subscribe(&input, move |this, _, event, cx| {
            if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                let title = input_clone.read(cx).text().to_string();
                let title = title.trim().to_string();
                if title.is_empty() {
                    return;
                }

                this.needs_clear = true;

                let store_opt = store_clone.read(cx).clone();
                let tasks_handle = tasks_clone.clone();
                let entity = entity.clone();
                cx.spawn(move |_, cx: &mut AsyncApp| {
                    let mut cx = cx.clone();
                    async move {
                        let Some(store) = store_opt else {
                            return;
                        };
                        {
                            let mut s = store.lock().await;
                            if let Err(e) =
                                s.create_task(storage::Task::create().title(title)).await
                            {
                                eprintln!("Failed to create task: {e}");
                            }
                        }
                        let new_tasks = {
                            let mut s = store.lock().await;
                            s.list_tasks().await.unwrap_or_default()
                        };
                        *tasks_handle.write().unwrap() = new_tasks;
                        entity.update(&mut cx, |_mini, cx| cx.notify());
                    }
                })
                .detach();
            }
        });

        Self {
            tasks,
            input,
            needs_clear: false,
            _subscription: subscription,
        }
    }
}

fn main() {
    let app = gpui_platform::application();

    app.run(move |cx| {
        gpui_tokio::init(cx);
        gpui_component::init(cx);

        let store_entity: Entity<Option<Arc<tokio::sync::Mutex<TodoStore>>>> = cx.new(|_cx| None);

        let tasks = Arc::new(RwLock::new(Vec::new()));

        let init_task = gpui_tokio::Tokio::spawn_result(cx, async move {
            let config = StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            let mut todo_store = TodoStore::new(&config).await?;
            todo_store.seed().await?;
            Ok::<_, anyhow::Error>(todo_store)
        });

        let store_entity2 = store_entity.clone();
        let tasks_init = tasks.clone();
        cx.spawn(move |cx: &mut AsyncApp| {
            let mut cx = cx.clone();
            async move {
                if let Ok(mut todo_store) = init_task.await {
                    let task_list = todo_store.list_tasks().await.unwrap_or_default();
                    *tasks_init.write().unwrap() = task_list;
                    store_entity2.update(&mut cx, |s, _cx| {
                        *s = Some(Arc::new(tokio::sync::Mutex::new(todo_store)));
                    });
                    cx.refresh();
                }
            }
        })
        .detach();

        cx.open_window(WindowOptions::default(), |window, cx| {
            Theme::change(ThemeMode::Dark, Some(window), cx);
            let input = cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                state.set_placeholder("New task...", window, cx);
                state
            });

            let mini = cx.new(|cx| MiniTodo::new(input, store_entity, tasks, cx));

            cx.new(|cx| {
                let mut root = gpui_component::Root::new(mini, window, cx);
                root.style().background = Some(rgb(0x1a1a1aff).into());
                root
            })
        })
        .expect("Failed to open window");
    });
}
