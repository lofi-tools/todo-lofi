use gpui::*;
use gpui_component::input::*;
use gpui_component::{StyledExt, Theme, ThemeMode};
use std::sync::Arc;
use storage::prelude::*;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

struct MiniTodo {
    tasks: Vec<storage::Task>,
    input: Entity<InputState>,
    store: Option<Arc<tokio::sync::Mutex<TodoStore>>>,
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
                            .id(("task", task.id))
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
    fn new(input: Entity<InputState>, cx: &mut Context<Self>) -> Self {
        let input_clone = input.clone();
        let subscription = cx.subscribe(&input, move |this, _, event, cx| {
            if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                tracing::info!("PressEnter received");
                let title = input_clone.read(cx).text().to_string();
                let title = title.trim().to_string();
                tracing::info!(title, "extracted from input");
                if title.is_empty() {
                    tracing::info!("title is empty, returning");
                    return;
                }
                this.insert_task(title, cx);
            }
        });

        Self {
            tasks: Vec::new(),
            input,
            store: None,
            needs_clear: false,
            insert_task: None,
            _subscription: subscription,
        }
    }

    fn insert_task(&mut self, title: String, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            tracing::error!("Store not initialized");
            return;
        };

        tracing::info!(title, "insert_task called");
        let task = gpui_tokio::Tokio::spawn(cx, async move {
            tracing::info!("Tokio: acquiring store lock...");
            let mut s = store.lock().await;
            tracing::info!("Tokio: creating task...");
            let _ = s
                .create_task(storage::Task::create().title(title))
                .await;
            tracing::info!("Tokio: listing tasks...");
            let tasks = s.list_tasks().await.unwrap_or_default();
            tracing::info!(count = tasks.len(), "Tokio: tasks fetched");
            Ok::<_, anyhow::Error>(tasks)
        });

        self.insert_task = Some(cx.spawn(async move |this, cx| {
            tracing::info!("spawn started, awaiting Tokio task...");
            match task.await {
                Ok(Ok(new_tasks)) => {
                    tracing::info!(count = new_tasks.len(), "Tokio task done, updating entity");
                    this.update(cx, |this, cx| {
                        this.tasks = new_tasks;
                        this.needs_clear = true;
                        tracing::info!("calling cx.notify()");
                        cx.notify();
                    })
                    .ok();
                }
                Ok(Err(e)) => {
                    tracing::error!("Failed to insert task: {e}");
                }
                Err(e) => {
                    tracing::error!("Tokio task panicked: {e}");
                }
            }
        }));
    }
}

fn init_logging() {
    let debug = std::env::args().any(|arg| arg == "--debug" || arg == "-d");
    let filter = if debug {
        EnvFilter::new("debug")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"))
    };
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_target(true))
        .with(filter)
        .init();
}

fn main() {
    init_logging();

    let app = gpui_platform::application();

    app.run(move |cx| {
        gpui_tokio::init(cx);
        gpui_component::init(cx);

        let init_task = gpui_tokio::Tokio::spawn(cx, async move {
            let config = StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            let mut store = TodoStore::new(&config).await?;
            store.seed().await?;
            let tasks = store.list_tasks().await.unwrap_or_default();
            Ok::<_, anyhow::Error>((Arc::new(tokio::sync::Mutex::new(store)), tasks))
        });

        cx.open_window(WindowOptions::default(), |window, cx| {
            Theme::change(ThemeMode::Dark, Some(window), cx);

            let input = cx.new(|cx| {
                let mut input_state = InputState::new(window, cx);
                input_state.set_placeholder("New task...", window, cx);
                input_state
            });

            let mini = cx.new(|cx| MiniTodo::new(input, cx));

            let entity = mini.clone();
            cx.spawn(move |cx: &mut AsyncApp| {
                let mut cx = cx.clone();
                let entity = entity.clone();
                async move {
                    match init_task.await {
                        Ok(Ok((store, tasks))) => {
                            entity.update(&mut cx, |mini, cx| {
                                mini.store = Some(store);
                                mini.tasks = tasks;
                                cx.notify();
                            });
                        }
                        Ok(Err(e)) => {
                            tracing::error!("Failed to initialize store: {e}");
                        }
                        Err(e) => {
                            tracing::error!("Store init task panicked: {e}");
                        }
                    }
                }
            })
            .detach();

            cx.new(|cx| gpui_component::Root::new(mini, window, cx))
        })
        .expect("Failed to open window");
    });
}
