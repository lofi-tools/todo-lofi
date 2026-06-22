use gpui::*;
use gpui_component::input::*;
use gpui_component::{StyledExt, Theme, ThemeMode};
use std::sync::Arc;
use storage::prelude::*;

struct MiniTodo {
    tasks: Vec<storage::Task>,
    input: Entity<InputState>,
    store: Arc<tokio::sync::Mutex<TodoStore>>,
    runtime_handle: tokio::runtime::Handle,
    needs_clear: bool,
    insert_task: Option<gpui::Task<()>>,
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

        let tasks = self.tasks.clone();

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
        store: Arc<tokio::sync::Mutex<TodoStore>>,
        runtime_handle: tokio::runtime::Handle,
        tasks: Vec<storage::Task>,
        cx: &mut Context<Self>,
    ) -> Self {
        let input_clone = input.clone();
        let subscription = cx.subscribe(&input, move |this, _, event, cx| {
            if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                eprintln!("[subscribe] PressEnter received");
                let title = input_clone.read(cx).text().to_string();
                let title = title.trim().to_string();
                eprintln!("[subscribe] title: '{title}'");
                if title.is_empty() {
                    return;
                }
                this.insert_task(title, cx);
            }
        });

        Self {
            tasks,
            input,
            store,
            runtime_handle,
            needs_clear: false,
            insert_task: None,
            _subscription: subscription,
        }
    }

    fn insert_task(&mut self, title: String, cx: &mut Context<Self>) {
        eprintln!("[insert_task] called with title: {title}");
        let store = self.store.clone();
        let handle = self.runtime_handle.clone();
        self.insert_task = Some(cx.spawn(async move |this, cx| {
            eprintln!("[insert_task] spawn started, awaiting Tokio task...");
            let new_tasks = handle
                .spawn(async move {
                    eprintln!("[insert_task] Tokio: acquiring store lock...");
                    let mut s = store.lock().await;
                    eprintln!("[insert_task] Tokio: creating task...");
                    let _ = s
                        .create_task(storage::Task::create().title(title))
                        .await;
                    eprintln!("[insert_task] Tokio: listing tasks...");
                    let tasks = s.list_tasks().await.unwrap_or_default();
                    eprintln!("[insert_task] Tokio: got {} tasks", tasks.len());
                    tasks
                })
                .await
                .unwrap();
            eprintln!(
                "[insert_task] Tokio task done, updating entity with {} tasks",
                new_tasks.len()
            );

            this.update(cx, |this, cx| {
                this.tasks = new_tasks;
                this.needs_clear = true;
                eprintln!("[insert_task] calling cx.notify()");
                cx.notify();
            })
            .ok();
        }));
    }
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let store = runtime.block_on(async {
        let config = StorageConfig {
            db_uri: "turso::memory:".to_string(),
        };
        let mut store = TodoStore::new(&config).await.unwrap();
        store.seed().await.unwrap();
        Arc::new(tokio::sync::Mutex::new(store))
    });

    let tasks =
        runtime.block_on(async { store.lock().await.list_tasks().await.unwrap_or_default() });

    let runtime_handle = runtime.handle().clone();

    let app = gpui_platform::application();

    app.run(move |cx| {
        gpui_component::init(cx);

        cx.open_window(WindowOptions::default(), |window, cx| {
            Theme::change(ThemeMode::Dark, Some(window), cx);

            let input = cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                state.set_placeholder("New task...", window, cx);
                state
            });

            let mini = cx.new(|cx| {
                MiniTodo::new(input, store.clone(), runtime_handle.clone(), tasks, cx)
            });

            cx.new(|cx| gpui_component::Root::new(mini, window, cx))
        })
        .expect("Failed to open window");
    });
}
