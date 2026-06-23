use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, rgb,
};
use gpui_component::StyledExt;
use gpui_component::input::*;
use storage::task::TaskCreate;

use super::navbar::{NavBar, NavBarEvent};
use super::task_view::TaskView;
use crate::store::Store;

pub struct TaskListView {
    task_views: Vec<Entity<TaskView>>,
    input: Entity<InputState>,
    store: Store,
    input_needs_clear: bool,
    _fetch_tasks: Option<gpui::Task<()>>,
    _input_subscription: Subscription,
    _nav_subscription: Subscription,
}

impl TaskListView {
    pub fn new(
        input: Entity<InputState>,
        store: Store,
        nav_bar: Entity<NavBar>,
        cx: &mut Context<Self>,
    ) -> Self {
        let input_clone = input.clone();
        let input_subscription = cx.subscribe(&input, move |this, _, event, cx| {
            if let gpui_component::input::InputEvent::PressEnter { .. } = event {
                let title = input_clone.read(cx).text().to_string();
                let title = title.trim().to_string();
                if title.is_empty() {
                    return;
                }
                this.insert_task(title, cx);
            }
        });

        let nav_store = store.clone();
        let nav_subscription =
            cx.subscribe(&nav_bar, move |this, _nav_bar, event, cx| match event {
                NavBarEvent::TagSelected(path) => {
                    let last = path.last().cloned().unwrap_or_default();
                    let store = nav_store.clone();
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
                            this.set_tasks(tasks, cx);
                            this._fetch_tasks = None;
                            cx.notify();
                        })
                        .ok();
                    });
                    this._fetch_tasks = Some(fetch);
                }
                NavBarEvent::AllTasks => {
                    let store = nav_store.clone();
                    let fetch = cx.spawn(async move |this, cx| {
                        let tasks = {
                            let mut s = store.0.lock().await;
                            s.list_tasks_by_priority().await.unwrap_or_default()
                        };
                        let tasks: Vec<_> = tasks.into_iter().map(|t| t.task).collect();
                        this.update(cx, |this, cx| {
                            this.set_tasks(tasks, cx);
                            this._fetch_tasks = None;
                            cx.notify();
                        })
                        .ok();
                    });
                    this._fetch_tasks = Some(fetch);
                }
            });

        Self {
            task_views: Vec::new(),
            input,
            store,
            input_needs_clear: false,
            _fetch_tasks: None,
            _input_subscription: input_subscription,
            _nav_subscription: nav_subscription,
        }
    }

    fn insert_task(&mut self, title: String, cx: &mut Context<Self>) {
        let create_task = self
            .store
            .insert_task(TaskCreate::default().title(title), cx);

        self._fetch_tasks = Some(cx.spawn(async move |this, cx| {
            let new_tasks = match create_task.await {
                Ok(new_tasks) => new_tasks,
                Err(e) => {
                    tracing::error!("Failed to insert task: {e}");
                    return;
                }
            };
            this.update(cx, |this, cx| {
                this.set_tasks(new_tasks, cx);
                this.input_needs_clear = true;
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn set_tasks(&mut self, tasks: Vec<storage::Task>, cx: &mut Context<Self>) {
        self.task_views = tasks
            .into_iter()
            .map(|task| cx.new(|cx| TaskView::new(task, self.store.clone(), cx)))
            .collect();
    }
}

impl Render for TaskListView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.input_needs_clear {
            self.input_needs_clear = false;
            self.input.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        }

        div()
            .flex_1()
            .v_flex()
            .p_8()
            .gap_4()
            .child(
                div()
                    .text_2xl()
                    .font_bold()
                    .text_color(rgb(0xe5e5e5))
                    .child("Mini Todo"),
            )
            .child(Input::new(&self.input))
            .child(
                div()
                    .flex_1()
                    .v_flex()
                    .gap_2()
                    .children(self.task_views.iter().cloned()),
            )
    }
}
