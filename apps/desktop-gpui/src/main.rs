use gpui::*;
use gpui::prelude::FluentBuilder;
use gpui_component::{input::*, *};
use std::collections::HashSet;
use storage::prelude::*;

use crate::{
    task_store::TaskStore,
    ui_parts::{navbar::NavBar, task_details::TaskDetails, task_list::TaskList},
};

pub mod ui_traits;
pub mod components {
    pub mod checkbox;
    pub mod tooltip;
}
pub mod task_store;
pub mod ui_parts {
    pub mod navbar;
    pub mod task_details;
    pub mod task_list;
}

#[derive(argh::FromArgs)]
/// todo-lofi desktop application
struct Args {
    /// path to the JSON configuration file
    #[argh(option)]
    config_path: Option<String>,

    /// pre-load hardcoded seed test data into the database
    #[argh(switch)]
    use_test_seed_data: bool,
}

#[derive(serde::Deserialize)]
struct AppConfig {
    db_uri: String,
}

impl AppConfig {
    fn load(path: &str) -> Self {
        let contents = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read config file `{path}`: {e}"));
        serde_json::from_str(&contents)
            .unwrap_or_else(|e| panic!("failed to parse config file `{path}`: {e}"))
    }
}

pub struct TodoApp {
    selected_task_id: Entity<Option<u64>>,
    sidebar_ui: Entity<NavBar>,
    task_list_ui: Entity<TaskList>,
    details_ui: Entity<TaskDetails>,
}

impl TodoApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, task_store: Entity<TaskStore>) -> Self {
        let input_state = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_placeholder("New task title...", window, cx);
            state
        });
        let selected_tag = cx.new(|_| None);
        let selected_path = cx.new(|_| Vec::new());
        let selected_task_id = cx.new(|_| None);
        let sidebar_ui = cx.new(|_| NavBar {
            task_store: task_store.clone(),
            selected_tag: selected_tag.clone(),
            selected_path,
        });
        let task_list_ui = cx.new(|_| TaskList {
            input_state: input_state.clone(),
            selected_tag: selected_tag.clone(),
            selected_task_id: selected_task_id.clone(),
            task_store: task_store.clone(),
            editing_index: None,
            excluded_tags: HashSet::new(),
        });
        let details_ui = cx.new(|_| TaskDetails {
            task_store: task_store.clone(),
            selected_task_id: selected_task_id.clone(),
        });

        Self {
            selected_task_id,
            sidebar_ui,
            task_list_ui,
            details_ui,
        }
    }
}

impl Render for TodoApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_details = self.selected_task_id.read(cx).is_some();

        div()
            .flex()
            .flex_row()
            .size_full()
            .bg(rgba(0x1e1e_1eff))
            .child(self.sidebar_ui.clone())
            .child(
                div()
                    .flex_1()
                    .h_flex()
                    .child(self.task_list_ui.clone())
                    .when(has_details, |this| this.child(self.details_ui.clone())),
            )
    }
}

fn main() {
    let args: Args = argh::from_env();

    let db_uri = match args.config_path {
        Some(path) => AppConfig::load(&path).db_uri,
        None => "turso::memory:".to_string(),
    };

    let app = gpui_platform::application().with_assets(gpui_component_assets::Assets);

    app.run(move |cx| {
        gpui_tokio::init(cx);
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);

        let task_store_entity = cx.new(|_| TaskStore::empty());

        let use_test_seed_data = args.use_test_seed_data;
        let init_task = gpui_tokio::Tokio::spawn_result(cx, async move {
            let config = StorageConfig { db_uri };
            let mut store = TodoStore::new(&config).await?;
            if use_test_seed_data {
                store.seed().await?;
            }
            TaskStore::load_from_storage(store).await
        });

        let entity = task_store_entity.clone();
        cx.spawn(move |cx: &mut gpui::AsyncApp| {
            let mut cx = cx.clone();
            let entity = entity.clone();
            async move {
                match init_task.await {
                    Ok(loaded) => {
                        entity.update(&mut cx, |store, _cx| *store = loaded);
                        cx.refresh();
                    }
                    Err(e) => {
                        eprintln!("Failed to initialize database: {e}");
                    }
                }
            }
        })
        .detach();

        cx.open_window(WindowOptions::default(), |window, cx| {
            let view = cx.new(|cx| TodoApp::new(window, cx, task_store_entity.clone()));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("Failed to open window");
    });
}
