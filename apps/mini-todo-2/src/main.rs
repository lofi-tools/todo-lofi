use gpui::{
    AppContext, AsyncApp, Context, Entity, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, Styled, Subscription, Task, Window, WindowOptions, div, px, rgb,
};
use gpui_component::input::*;
use gpui_component::{StyledExt, Theme, ThemeMode};
use std::collections::HashMap;
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

    fn list_top_level_tags(&self, cx: &impl AppContext) -> Task<anyhow::Result<Vec<Tag>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let tags = s.get_top_level_tags().await.unwrap_or_default();
            Ok(tags)
        })
    }

    fn get_children(&self, tag_id: u64, cx: &impl AppContext) -> Task<anyhow::Result<Vec<Tag>>> {
        let store = self.0.clone();
        gpui_tokio::Tokio::spawn_result(cx, async move {
            let mut s = store.lock().await;
            let children = s.get_children(tag_id).await.unwrap_or_default();
            Ok(children)
        })
    }
}

struct NavBar {
    store: Store,
    top_level_tags: Vec<Tag>,
    children_cache: HashMap<u64, Vec<Tag>>,
    selected_path: Vec<String>,
    _fetch_tags: Option<Task<()>>,
    _fetch_children: Option<Task<()>>,
}

impl NavBar {
    fn new(store: Store, cx: &mut Context<Self>) -> Self {
        let fetch_store = store.clone();
        let fetch_task = fetch_store.list_top_level_tags(cx);

        let _fetch_tags = Some(cx.spawn(async move |this, cx| match fetch_task.await {
            Ok(tags) => {
                this.update(cx, |this, cx| {
                    this.top_level_tags = tags;
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
            store,
            top_level_tags: Vec::new(),
            children_cache: HashMap::new(),
            selected_path: Vec::new(),
            _fetch_tags,
            _fetch_children: None,
        }
    }

    fn navigate_to_tag(&mut self, tag_name: &str, _tag_id: u64, path: &[String], cx: &mut Context<Self>) {
        if self.selected_path == path {
            self.selected_path.retain(|p| p != tag_name);
            cx.notify();
            return;
        }
        self.selected_path = path.to_vec();

        let mut current_children = self.top_level_tags.clone();
        for name in &self.selected_path {
            if let Some(tag) = current_children.iter().find(|t| t.name == *name) {
                if !self.children_cache.contains_key(&tag.id) {
                    let store = self.store.clone();
                    let id = tag.id;
                    let fetch_task = store.get_children(id, cx);
                    self._fetch_children = Some(cx.spawn(async move |this, cx| {
                        match fetch_task.await {
                            Ok(children) => {
                                this.update(cx, |this, cx| {
                                    this.children_cache.insert(id, children);
                                    this._fetch_children = None;
                                    cx.notify();
                                })
                                .ok();
                            }
                            Err(e) => {
                                tracing::error!("Failed to fetch children: {e}");
                            }
                        }
                    }));
                    break;
                }
                current_children = self.children_cache.get(&tag.id).cloned().unwrap_or_default();
            }
        }
        cx.notify();
    }

    fn collect_visible_tags(&self) -> Vec<(String, u64, usize, bool, Vec<String>)> {
        let mut result = Vec::new();

        fn walk(
            tag: &Tag,
            depth: usize,
            ancestors: &[String],
            selected_path: &[String],
            children_cache: &HashMap<u64, Vec<Tag>>,
            result: &mut Vec<(String, u64, usize, bool, Vec<String>)>,
        ) {
            let children = children_cache.get(&tag.id).cloned().unwrap_or_default();
            let has_children = !children.is_empty();
            let mut path = ancestors.to_vec();
            path.push(tag.name.clone());
            result.push((tag.name.clone(), tag.id, depth, has_children, path.clone()));

            let is_on_path = selected_path.iter().any(|p| p == &tag.name);
            if is_on_path {
                for child in &children {
                    walk(child, depth + 1, &path, selected_path, children_cache, result);
                }
            }
        }

        for tag in &self.top_level_tags {
            walk(tag, 0, &[], &self.selected_path, &self.children_cache, &mut result);
        }

        result
    }
}

impl Render for NavBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let visible_tags = self.collect_visible_tags();

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
            .children(visible_tags.into_iter().map(|(tag_name, tag_id, depth, _has_children, path)| {
                let tag_for_click = tag_name.clone();
                let path_for_click = path;

                div()
                    .h_flex()
                    .items_center()
                    .ml(px(depth as f32 * 8.0))
                    .child(
                        div()
                            .flex_1()
                            .child(tag_name)
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .hover(|s| s.bg(rgb(0x2a2a2a)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.navigate_to_tag(&tag_for_click, tag_id, &path_for_click, cx);
                                }),
                            ),
                    )
            }))
    }
}

struct TaskList {
    tasks: Vec<storage::Task>,
    input: Entity<InputState>,
    store: Store,
    needs_clear: bool,
    _insert_task: Option<gpui::Task<()>>,
    _subscription: Subscription,
}

impl TaskList {
    fn new(input: Entity<InputState>, store: Store, cx: &mut Context<Self>) -> Self {
        let input_clone = input.clone();
        let subscription = cx.subscribe(&input, move |this, _, event, cx| {
            if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                let title = input_clone.read(cx).text().to_string();
                let title = title.trim().to_string();
                if title.is_empty() {
                    return;
                }
                this.insert_task(title, cx);
            }
        });

        Self {
            tasks: Vec::new(),
            input,
            store,
            needs_clear: false,
            _insert_task: None,
            _subscription: subscription,
        }
    }

    fn insert_task(&mut self, title: String, cx: &mut Context<Self>) {
        let create_task = self
            .store
            .insert_task(TaskCreate::default().title(title), cx);

        self._insert_task = Some(cx.spawn(async move |this, cx| {
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

    pub fn set_tasks(&mut self, tasks: Vec<storage::Task>) {
        self.tasks = tasks;
    }
}

impl Render for TaskList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.needs_clear {
            self.needs_clear = false;
            self.input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }

        let tasks = self.tasks.clone();

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
            )
    }
}

struct Layout {
    task_list: Entity<TaskList>,
    nav_bar: Entity<NavBar>,
}

impl Layout {
    fn new(input: Entity<InputState>, store: Store, cx: &mut Context<Self>) -> Self {
        let nav_bar = cx.new(|cx| NavBar::new(store.clone(), cx));
        let task_list = cx.new(|cx| TaskList::new(input, store, cx));

        Self { task_list, nav_bar }
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
            let tasks = store.list_tasks().await.unwrap_or_default();
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
