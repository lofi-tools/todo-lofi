use gpui::{
    AppContext, AsyncApp, Context, Entity, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, WindowOptions, div, px, rgb,
};
use gpui_component::StyledExt;
use gpui_component::input::*;
use gpui_component::{Theme, ThemeMode};
use storage::prelude::*;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

use projects::Project;
use store::Store;
use ui_parts::navbar::NavBar;
use ui_parts::task_list::TaskListView;

mod components;
mod projects;
mod store;
mod ui_parts {
    pub mod navbar;
    pub mod task_list;
    pub mod task_row;
}

struct Layout {
    pub task_list: Entity<TaskListView>,
    nav_bar: Entity<NavBar>,
    /// Repos found in the home directory scan, kept here so the details pane
    /// can look one up when a nav row is clicked.
    _projects: Vec<Project>,
    /// Rendered details-pane content for the selected project, if any.
    project_details: Option<String>,
    _project_subscription: Subscription,
}

impl Layout {
    fn new(input: Entity<InputState>, store: Store, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let nav_bar = cx.new(|cx| NavBar::new(store.clone(), cx));        // Kick off the home-directory repo scan in the background; the nav
        // bar fills in its Projects section when it lands.
        let scan = projects::scan(cx);
        cx.spawn(async move |this, cx| {
            let projects = match scan.await {
                Ok(projects) => projects,
                Err(e) => {
                    tracing::error!("Failed to scan for git repos: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                this.nav_bar
                    .update(cx, |nav, cx| nav.set_projects(projects.clone(), cx));
                this._projects = projects;
                cx.notify();
            })
            .ok();
        })
        .detach();

        // Clicking a project row in the nav bar fetches its details (branch,
        // dirty-file count) and shows them in the right-hand pane.
        let project_subscription = cx.subscribe_in(
            &nav_bar,
            window,
            |_this, _nav, event, _window, cx| {
                if let ui_parts::navbar::NavBarEvent::ProjectSelected(project) = event {
                    let describe = project.describe(cx);
                    cx.spawn(async move |this, cx| {
                        let details = match describe.await {
                            Ok(details) => details,
                            Err(e) => {
                                tracing::error!("Failed to describe project: {e}");
                                return;
                            }
                        };
                        this.update(cx, |this, cx| {
                            this.project_details = Some(details);
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                }
            },
        );
        let task_list = cx.new(|cx| TaskListView::new(input, store.clone(), nav_bar.clone(), cx));

        Self {
            task_list,
            nav_bar,
            _projects: Vec::new(),
            project_details: None,
            _project_subscription: project_subscription,
        }
    }
}

impl Render for Layout {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .size_full()
            .child(div().w(px(256.)).flex_none().child(self.nav_bar.clone()))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .child(div().flex_1().child(self.task_list.clone()))
                    .child(
                        div()
                            .flex_1()
                            .p_8()
                            .v_flex()
                            .gap_1()
                            .child(match &self.project_details {
                                Some(details) => div().v_flex().gap_1().children(
                                    details
                                        .lines()
                                        .map(|line| {
                                            div().text_color(rgb(0xe5e5e5)).child(line.to_string())
                                        })
                                        .collect::<Vec<_>>(),
                                ),
                                None => div()
                                    .text_color(rgb(0x666666))
                                    .child("Select a project to see details"),
                            }),
                    ),
            )
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

                            let mini = cx.new(|cx| Layout::new(input, store, window, cx));

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
