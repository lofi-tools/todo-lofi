use gpui::{
    AppContext, AsyncApp, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, WindowOptions, div, rgb,
};
use gpui_component::input::*;
use gpui_component::{Theme, ThemeMode};
use storage::prelude::*;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use store::Store;
use ui_parts::navbar::{NavBar, NavBarEvent};
use ui_parts::task_list::TaskList;

mod store;
mod ui_parts {
    pub mod navbar;
    pub mod task_list;
}

struct Layout {
    task_list: Entity<TaskList>,
    nav_bar: Entity<NavBar>,
    _fetch_tasks: Option<gpui::Task<()>>,
    _subscription: Subscription,
}

impl Layout {
    fn new(input: Entity<InputState>, store: Store, cx: &mut Context<Self>) -> Self {
        let nav_bar = cx.new(|cx| NavBar::new(store.clone(), cx));
        let task_list = cx.new(|cx| TaskList::new(input, store.clone(), cx));

        let subscribe_store = store.clone();
        let subscription = cx.subscribe(&nav_bar, move |this, _nav_bar, event, cx| match event {
            NavBarEvent::TagSelected(path) => {
                let last = path.last().cloned().unwrap_or_default();
                let store = subscribe_store.clone();
                let fetch = cx.spawn(async move |this, cx| {
                    let tag_id = {
                        let mut s = store.0.lock().await;
                        s.get_tag_by_name(&last).await.ok().flatten().map(|t| t.id)
                    };
                    let Some(tag_id) = tag_id else {
                        return;
                    };
                    let tasks = {
                        let mut s = store.0.lock().await;
                        s.list_tasks_by_tag(tag_id).await.unwrap_or_default()
                    };
                    let tasks: Vec<_> = tasks.into_iter().map(|t| t.task).collect();
                    this.update(cx, |this, cx| {
                        this.task_list.update(cx, |list, _| list.set_tasks(tasks));
                        this._fetch_tasks = None;
                        cx.notify();
                    })
                    .ok();
                });
                this._fetch_tasks = Some(fetch);
            }
            NavBarEvent::AllTasks => {
                let store = subscribe_store.clone();
                let fetch = cx.spawn(async move |this, cx| {
                    let tasks = {
                        let mut s = store.0.lock().await;
                        s.list_tasks_by_priority().await.unwrap_or_default()
                    };
                    let tasks: Vec<_> = tasks.into_iter().map(|t| t.task).collect();
                    this.update(cx, |this, cx| {
                        this.task_list.update(cx, |list, _| list.set_tasks(tasks));
                        this._fetch_tasks = None;
                        cx.notify();
                    })
                    .ok();
                });
                this._fetch_tasks = Some(fetch);
            }
        });

        Self {
            task_list,
            nav_bar,
            _fetch_tasks: None,
            _subscription: subscription,
        }
    }

    pub fn set_tasks(&mut self, tasks: Vec<storage::Task>, cx: &mut Context<Self>) {
        self.task_list.update(cx, |list, _| list.set_tasks(tasks));
    }
}

impl Render for Layout {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .size_full()
            .child(self.nav_bar.clone())
            .child(self.task_list.clone())
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

        let init_store = gpui_tokio::Tokio::spawn_result(cx, async move {
            let config = StorageConfig {
                db_uri: "turso::memory:".to_string(),
            };
            let mut store = TodoStore::new(&config).await?;
            store.seed().await?;
            let tasks_with_meta = store.list_tasks_by_priority().await.unwrap_or_default();
            let tasks = tasks_with_meta.into_iter().map(|t| t.task).collect();
            Ok::<_, anyhow::Error>((Store::new(store), tasks))
        });

        cx.spawn(|cx: &mut AsyncApp| {
            let cx = cx.clone();
            async move {
                match init_store.await {
                    Ok((store, tasks)) => {
                        cx.open_window(WindowOptions::default(), |window, cx| {
                            Theme::change(ThemeMode::Dark, Some(window), cx);

                            let input = cx.new(|cx| {
                                let mut input_state = InputState::new(window, cx);
                                input_state.set_placeholder("New task...", window, cx);
                                input_state
                            });

                            let mini = cx.new(|cx| Layout::new(input, store, cx));

                            let entity = mini.clone();
                            cx.spawn(move |cx: &mut AsyncApp| {
                                let mut cx = cx.clone();
                                let entity = entity.clone();
                                async move {
                                    entity.update(&mut cx, |mini, cx| {
                                        mini.set_tasks(tasks, cx);
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
