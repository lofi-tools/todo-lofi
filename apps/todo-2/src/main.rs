use gpui::{
    AppContext, AsyncApp, Context, Entity, IntoElement, ParentElement, Render, Styled, Window,
    WindowOptions, div, rgb,
};
use gpui_component::input::*;
use gpui_component::{Theme, ThemeMode};
use storage::prelude::*;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use store::Store;
use ui_parts::navbar::NavBar;
use ui_parts::task_list::TaskListView;

mod components;
mod store;
mod ui_parts {
    pub mod navbar;
    pub mod task_list;
    pub mod task_row;
}

struct Layout {
    pub task_list: Entity<TaskListView>,
    nav_bar: Entity<NavBar>,
}

impl Layout {
    fn new(input: Entity<InputState>, store: Store, cx: &mut Context<Self>) -> Self {
        let nav_bar = cx.new(|cx| NavBar::new(store.clone(), cx));
        let task_list = cx.new(|cx| TaskListView::new(input, store.clone(), nav_bar.clone(), cx));

        Self { task_list, nav_bar }
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
            let tasks = store.list_tasks_by_priority().await.unwrap_or_default();
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
                                        mini.task_list
                                            .update(cx, |list, cx| list.set_tasks(tasks, cx));
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
