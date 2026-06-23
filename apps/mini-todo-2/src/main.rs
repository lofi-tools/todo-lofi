use gpui::{
    AppContext, AsyncApp, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    Styled, Subscription, Task, Window, WindowOptions, div, rgb,
};
use gpui_component::input::*;
use gpui_component::{StyledExt, Theme, ThemeMode};
use std::sync::Arc;
use storage::prelude::*;
use storage::task::TaskCreate;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

#[derive(Clone)]
struct Store(Arc<tokio::sync::Mutex<TodoStore>>);

impl Store {
    fn new(store: TodoStore) -> Self {
        Store(Arc::new(tokio::sync::Mutex::new(store)))
    }

    fn insert_task(
        &self,
        create: TaskCreate,
        cx: &impl AppContext,
    ) -> gpui::Task<anyhow::Result<Vec<storage::Task>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let _ = s.create_task(create).await;
            let tasks = s.list_tasks().await.unwrap_or_default();
            Ok(tasks)
        })
    }

    fn list_tags(&self, cx: &impl AppContext) -> Task<anyhow::Result<Vec<Tag>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let tags = s.list_tags().await.unwrap_or_default();
            Ok(tags)
        })
    }
}

struct NavBar {
    _store: Store,
    cached_tags: Vec<Tag>,
    _fetch_tags: Option<Task<()>>,
}

impl NavBar {
    fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let fetch_store = store.clone();
        let fetch_task = fetch_store.list_tags(cx);

        let _fetch_tags = Some(cx.spawn(async move |this, cx| match fetch_task.await {
            Ok(tags) => {
                tracing::info!("Fetched {:?} tags", tags);
                this.update(cx, |this, cx| {
                    this.cached_tags = tags;
                    this._fetch_tags = None;
                    cx.notify();
                })
                .ok();
            }
            Err(e) => {
                tracing::error!("Failed to fetch tags: {e}");
            }
        }));

        Self {
            _store: store,
            cached_tags: Vec::new(),
            _fetch_tags,
        }
    }
}

impl Render for NavBar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let tags = self.cached_tags.clone();

        div()
            .w_64()
            .flex_none()
            .h_full()
            .bg(rgb(0x1e1e1e))
            .border_r_1()
            .border_color(rgb(0x333333))
            .p_4()
            .v_flex()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_semibold()
                    .text_color(rgb(0xa3a3a3))
                    .mb_2()
                    .child("Tags"),
            )
            .child(
                div()
                    .child("All Tasks")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .hover(|s| s.bg(rgb(0x2a2a2a))),
            )
            .children(tags.into_iter().map(|tag| {
                div()
                    .id(("tag", tag.id))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .hover(|s| s.bg(rgb(0x2a2a2a)))
                    .child(tag.name)
            }))
    }
}

struct MiniTodo {
    tasks: Vec<storage::Task>,
    input: Entity<InputState>,
    nav_bar: Entity<NavBar>,
    store: Store,
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
            .flex_row()
            .size_full()
            .child(self.nav_bar.clone())
            .child(
                div()
                    .flex_1()
                    .v_flex()
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
                    ),
            )
    }
}

impl MiniTodo {
    fn new(input: Entity<InputState>, store: Store, cx: &mut Context<Self>) -> Self {
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

        let nav_bar = cx.new(|cx| NavBar::new(store.clone(), cx));

        Self {
            tasks: Vec::new(),
            input,
            nav_bar,
            store,
            needs_clear: false,
            insert_task: None,
            _subscription: subscription,
        }
    }

    fn insert_task(&mut self, title: String, cx: &mut Context<Self>) {
        let create_task = self
            .store
            .insert_task(TaskCreate::default().title(title), cx);

        self.insert_task = Some(cx.spawn(async move |this, cx| {
            let new_tasks = match create_task.await {
                Ok(new_tasks) => new_tasks,
                Err(e) => {
                    tracing::error!("Failed to insert task: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                this.tasks = new_tasks;
                this.needs_clear = true;
                cx.notify();
            })
            .ok();
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

        let init_task = gpui_tokio::Tokio::spawn_result(cx, async move {
            let config = StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            let mut store = TodoStore::new(&config).await?;
            store.seed().await?;
            let tasks = store.list_tasks().await.unwrap_or_default();
            Ok::<_, anyhow::Error>((Store::new(store), tasks))
        });

        cx.spawn(|cx: &mut AsyncApp| {
            let cx = cx.clone();
            async move {
                match init_task.await {
                    Ok((store, tasks)) => {
                        cx.open_window(WindowOptions::default(), |window, cx| {
                            Theme::change(ThemeMode::Dark, Some(window), cx);

                            let input = cx.new(|cx| {
                                let mut input_state = InputState::new(window, cx);
                                input_state.set_placeholder("New task...", window, cx);
                                input_state
                            });

                            let mini = cx.new(|cx| MiniTodo::new(input, store, cx));

                            let entity = mini.clone();
                            cx.spawn(move |cx: &mut AsyncApp| {
                                let mut cx = cx.clone();
                                let entity = entity.clone();
                                async move {
                                    entity.update(&mut cx, |mini, cx| {
                                        mini.tasks = tasks;
                                        cx.notify();
                                    });
                                }
                            })
                            .detach();

                            cx.new(|cx| {
                                gpui_component::Root::new(mini, window, cx).bg(rgb(0x1a1a1a))
                            })
                        })
                        .expect("Failed to open window");
                    }
                    Err(e) => {
                        tracing::error!("Failed to initialize store: {e}");
                    }
                }
            }
        })
        .detach();
    });
}
