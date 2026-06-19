pub mod utils;
pub use crate::utils::*;

use dson::{
    CausalContext, CausalDotStore, Dot, DotChange, DotFun, DotStore, DotStoreJoin, DryJoinOutput,
    Identifier, sentinel::Sentinel,
};
use entity_id::EntityId;
use ulid::Ulid;

#[derive(EntityId, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
#[entity_id(prefix = "task")]
pub struct TaskId(Ulid);

pub type ActorId = dson::Identifier;

#[derive(Debug, Clone)]
pub struct Delta<T>(pub CausalDotStore<T>);
impl<T> std::ops::Deref for Delta<T> {
    type Target = CausalDotStore<T>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A custom CRDT struct.
/// DotFun maps a Dot (unique event ID) to a Value.
/// DotFun fields can contain multiple values during a conflict, which we resolve in `join`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Task {
    pub id: TaskId,
    pub title: DotFun<String>,
    pub done: DotFun<bool>,
}
impl DotStore for Task {
    fn add_dots_to(&self, other: &mut CausalContext) {
        self.title.add_dots_to(other);
        self.done.add_dots_to(other);
    }
    fn is_bottom(&self) -> bool {
        self.title.is_bottom() && self.done.is_bottom()
    }
    fn subset_for_inflation_from(&self, frontier: &CausalContext) -> Self {
        Self {
            id: self.id,
            title: self.title.subset_for_inflation_from(frontier),
            done: self.done.subset_for_inflation_from(frontier),
        }
    }
}
impl Task {
    // pub fn new_for(cc: &CausalContext, actor: ActorId, title: &str) -> Self {
    //     let dot = cc.next_dot_for(actor);
    //     Self {
    //         id: TaskId::new(),
    //         title: {
    //             let mut val = DotFun::default();
    //             val.set(dot, title.to_string());
    //             val
    //         },
    //         done: DotFun::default(),
    //     }
    // }
    pub fn new_at_dot(dot: Dot, title: &str) -> Self {
        Self {
            id: TaskId::new(),
            title: {
                let mut val = DotFun::default();
                val.set(dot, title.to_string());
                val
            },
            done: DotFun::default(),
        }
    }
}

// impl<S> DotStoreJoin<S> for Task
// where
//     S: dson::sentinel::Sentinel
//         + dson::sentinel::ValueSentinel<String>
//         + dson::sentinel::ValueSentinel<bool>,
// {
//     fn join(
//         (s1, c1): (Self, &CausalContext),
//         (s2, c2): (Self, &CausalContext),
//         on_dot_change: &mut dyn FnMut(DotChange),
//         sentinel: &mut S,
//     ) -> Result<Self, S::Error> {
//         if s1.id != s2.id {
//             return Ok(s1); // Should be error: cannot join tasks with different ids
//         }

//         // Join field field
//         let mut new_title =
//             DotStoreJoin::join((s1.title, c1), (s2.title, c2), on_dot_change, sentinel)?;
//         let mut new_done =
//             DotStoreJoin::join((s1.done, c1), (s2.done, c2), on_dot_change, sentinel)?;

//         // Conflict Resolution (LWW - Last Writer Wins)
//         // If concurrent writes caused multiple values (dots) to exist in a field,
//         // we pick the one with the "maximum" dot (highest seq, then highest ID).
//         // This ensures the field acts as a "Single-Value Register".
//         if new_title.len() > 1
//             && let Some((dot, val)) = new_title.iter().max_by_key(|(d, _)| *d)
//         {
//             let winner = val.clone();
//             let winner_dot = dot;
//             new_title = DotFun::default();
//             new_title.set(winner_dot, winner);
//         }

//         if new_done.len() > 1
//             && let Some((dot, val)) = new_done.iter().max_by_key(|(d, _)| *d)
//         {
//             let winner = *val;
//             let winner_dot = dot;
//             new_done = DotFun::default();
//             new_done.set(winner_dot, winner);
//         }

//         Ok(Task {
//             id: s1.id,
//             title: new_title,
//             done: new_done,
//         })
//     }
// }

impl DotStoreJoin<TaskValidator> for Task {
    fn join(
        (ds1, c1): (Self, &CausalContext),
        (ds2, c2): (Self, &CausalContext),
        on_dot_change: &mut dyn FnMut(DotChange),
        sentinel: &mut TaskValidator,
    ) -> Result<Self, StrErr> {
        if ds1 == Self::default() {
            return Ok(ds2);
        }
        if ds2 == Self::default() {
            return Ok(ds1);
        }
        if ds1.id != TaskId::default() && ds2.id != TaskId::default() && ds1.id != ds2.id {
            return Err(format!("ID mismatch: \"{}\" != \"{}\"", ds1.id, ds2.id).into());
        }
        Ok(Self {
            id: ds1.id,
            title: DotStoreJoin::join((ds1.title, c1), (ds2.title, c2), on_dot_change, sentinel)?,
            done: DotStoreJoin::join((ds1.done, c1), (ds2.done, c2), on_dot_change, sentinel)?,
        })
    }
    fn dry_join(
        (s1, c1): (&Self, &CausalContext),
        (s2, c2): (&Self, &CausalContext),
        sentinel: &mut TaskValidator,
    ) -> Result<DryJoinOutput, StrErr> {
        // check if any field would change
        let title_out = DotStoreJoin::dry_join((&s1.title, c1), (&s2.title, c2), sentinel)?;
        let done_out = DotStoreJoin::dry_join((&s1.done, c1), (&s2.done, c2), sentinel)?;
        // If either field changes, the struct changes
        Ok(DryJoinOutput::new(
            title_out.is_bottom() && done_out.is_bottom(),
        ))
    }
}

impl Task {
    /// Creates a delta to set the title.
    pub fn delta_set_title(&self, value: &str, cc: &CausalContext, id: Identifier) -> Delta<Self> {
        let next_dot = cc.next_dot_for(id);

        let mut title_delta = DotFun::default();
        title_delta.set(next_dot, value.to_string());

        let task_delta = Task {
            id: self.id,
            title: title_delta,
            done: DotFun::default(), // No change to 'done'
        };

        // Create the delta context.
        // We MUST include the dots we are aware of for this field.
        // This acts as the "previous operation" dependency.
        let mut delta_cc = CausalContext::default();
        // Include old dots from this field so they can be tombstoned/replaced
        // self.title.add_dots_to(&mut delta_cc);
        // Include the new dot
        delta_cc.insert_dot(next_dot);

        Delta(CausalDotStore {
            store: task_delta,
            context: delta_cc,
        })
    }

    // /// Creates a delta to set the done status.
    // pub fn set_done(
    //     &self,
    //     value: bool,
    //     current_ctx: &CausalContext,
    //     id: Identifier,
    // ) -> CausalDotStore<Self> {
    //     let next_seq = current_ctx
    //         .iter()
    //         .filter_map(|(i, s)| if i == id { Some(s) } else { None })
    //         .max()
    //         .map_or(1, |s| s + 1);

    //     let dot = Dot::new(id, next_seq);

    //     let mut done_delta = DotFun::default();
    //     done_delta.set(dot, value);

    //     let task_delta = Task {
    //         title: DotFun::default(),
    //         done: done_delta,
    //     };

    //     let mut delta_ctx = CausalContext::default();
    // self.done.add_dots_to(&mut delta_ctx); // Include history of 'done'
    //     delta_ctx.insert(dot);

    //     CausalDotStore {
    //         store: task_delta,
    //         context: delta_ctx,
    //     }
    // }

    /// Helper to get the current resolved title
    pub fn get_title(&self) -> Option<&String> {
        // Since we resolve conflicts in join, there should be at most one value.
        self.title.values().next()
    }

    /// Helper to get the current resolved done status
    pub fn get_done(&self) -> Option<bool> {
        self.done.values().next().copied()
    }
}

pub struct TaskValidator;
impl dson::sentinel::Sentinel for TaskValidator {
    type Error = StrErr;
}
impl<K> dson::sentinel::ValueSentinel<K> for TaskValidator {}
impl dson::sentinel::KeySentinel for TaskValidator {}
impl<K> dson::sentinel::Visit<K> for TaskValidator {
    fn enter(&mut self, _key: &K) -> Result<(), Self::Error> {
        Ok(())
    }
    fn exit(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
pub type TaskJoinErr = <TaskValidator as Sentinel>::Error;

// pub mod single_task {
//     use crate::task::{ActorId, Delta, Task, Validator};
//     use dson::{CausalDotStore, Identifier};

//     #[derive(Debug, PartialEq)]
//     pub struct SingleTaskStore {
//         store: CausalDotStore<Task>,
//         owner: ActorId,
//     }
//     impl SingleTaskStore {
//         pub fn new(owner: Identifier) -> Self {
//             Self {
//                 store: CausalDotStore::default(),
//                 owner,
//             }
//         }
//         pub fn mut_set_title(&mut self, title: &str) -> Result<Delta<Task>, String> {
//             let title_delta =
//                 Task::delta_set_title(&self.store.store, title, &self.store.context, self.owner);

//             self.store.clone().join(title_delta.0, &mut Validator {})?;

//             // apply the delta to the store
//             // self.store
//             //     .join_or_replace_with(title_delta.store.clone(), &title_delta.context);
//             // DotStoreJoin::join(
//             //     &mut self.store,
//             //     title_delta.store.clone(),
//             //     &title_delta.context,
//             // );
//             Ok(title_delta)
//         }
//         pub fn get_title(&self) -> Option<&String> {
//             self.store.store.get_title()
//         }
//         pub fn join(&mut self, other: Delta<Task>) -> Result<(), String> {
//             self.store
//                 .join_with(other.0.store, &other.0.context, &mut Validator {})?;
//             Ok(())
//         }
//     }

//     #[cfg(test)]
//     mod tests {
//         use crate::StrErr;

//         use super::*;

//         // #[test]
//         // fn test_set_title() {
//         //     let alice = Identifier::new(0, 0);
//         //     let mut store_alice = SingleTaskStore::new(alice);
//         //     store_alice.mut_set_title("Test Task");
//         //     assert_eq!(
//         //         store_alice.get_title().cloned(),
//         //         Some("Test Task".to_string())
//         //     );
//         // }

//         #[test]
//         fn test_concurrent_set_title() -> Result<(), StrErr> {
//             let alice = Identifier::new(0, 0);
//             let bob = Identifier::new(1, 0);

//             let mut store_alice = SingleTaskStore::new(alice);
//             // deltas are applied to the store in set_title()
//             let delta1_alice = store_alice.mut_set_title("Alice title");
//             assert_eq!(
//                 store_alice.get_title().cloned(),
//                 Some("Alice title".to_string())
//             );

//             let mut store_bob = SingleTaskStore::new(bob);
//             let delta1_bob = store_bob.mut_set_title("Bob title");
//             assert_eq!(
//                 store_bob.get_title().cloned(),
//                 Some("Bob title".to_string())
//             );

//             // Sync: exchange deltas
//             store_alice.join(delta1_bob)?;
//             store_bob.join(delta1_alice)?;
//             // dbg!(&store_alice, &store_bob);
//             assert_eq!(store_alice.store.store, store_bob.store.store);

//             let delta2_bob = store_bob.mut_set_title("Bob change title again");
//             store_alice.join(delta2_bob)?;
//             assert_eq!(
//                 store_alice.get_title().cloned(),
//                 Some("Bob change title again".to_string())
//             );
//             assert_eq!(
//                 store_bob.get_title().cloned(),
//                 Some("Bob change title again".to_string())
//             );

//             Ok(())
//         }

//         // #[test]
//         // fn test_set_done() {
//         //     let id = Identifier::new(0, 0);
//         //     let mut task = Task::default();
//         //     let current_ctx = CausalContext::default();
//         //     let done_delta = task.set_done(true, &current_ctx, id);
//         //     assert_eq!(done_delta.store.done.get(&Dot::new(id, 1)), Some(&true));
//         //     assert_eq!(done_delta.context.len(), 1);
//         // }
//     }
// }

// pub mod task_map {
//     use super::*;

//     use crate::{
//         StrErr,
//         task::{ActorId, Delta, Task, TaskId, Validator, ValidatorErr},
//     };
//     use dson::{CausalContext, CausalDotStore, DotMap, DotStore, Identifier};

//     pub struct TaskMap(pub DotMap<TaskId, Task>);
//     impl std::ops::Deref for TaskMap {
//         type Target = DotMap<TaskId, Task>;
//         fn deref(&self) -> &Self::Target {
//             &self.0
//         }
//     }
//     impl std::ops::DerefMut for TaskMap {
//         fn deref_mut(&mut self) -> &mut Self::Target {
//             &mut self.0
//         }
//     }
//     impl TaskMap {
//         pub fn delta_add_task(
//             &mut self,
//             task_title: &str,
//             cc: &CausalContext,
//             id: Identifier,
//         ) -> Delta<Self> {
//             let next_dot = cc.next_dot_for(id);

//             // create the delta data
//             let mut delta_data = DotMap::default();
//             let task = Task::new_at_dot(next_dot, task_title);
//             delta_data.set(task.id, task);

//             // then create the delta causal context
//             let mut delta_cc = CausalContext::new();
//             // Include old dots so they can be tombstoned/replaced
//             // self.0.add_dots_to(&mut delta_cc);
//             // Include the new dot
//             delta_cc.insert_dot(next_dot);

//             Delta(CausalDotStore {
//                 store: TaskMap(delta_data),
//                 context: delta_cc,
//             })
//         }
//     }

//     #[derive(Debug, Clone)]
//     pub struct TaskMapStore {
//         store: CausalDotStore<DotMap<TaskId, Task>>,
//         owner: ActorId,
//     }
//     impl TaskMapStore {
//         pub fn new(owner: Identifier) -> Self {
//             Self {
//                 store: CausalDotStore::default(),
//                 owner,
//             }
//         }
//         pub fn join(
//             self,
//             other: &Delta<DotMap<TaskId, Task>>,
//         ) -> Result<DotMap<TaskId, Task>, ValidatorErr> {
//             // self.store
//             //     .join_or_replace_with(other.store.clone(), &other.context)
//             let joined = self.store.join(other.0.clone(), &mut Validator {})?;
//             Ok(joined.store)
//             // DotStoreJoin::join(
//             //     (self.store.store, &self.store.context),
//             //     (other.store.clone(), &other.context),
//             //     &mut |_change| {},
//             //     &mut DummySentinel,
//             // )
//         }
//         // pub fn map(&self) -> &DotMap<TaskId, Task> {
//         //     &self.store.store
//         // }
//         // pub fn context(&mut self) -> &mut CausalContext {
//         //     &mut self.store.context
//         // }

//         // pub fn new_task(&mut self, title: &str) -> Delta<DotMap<TaskId, Task>> {
//         //     pub fn new(cc: &CausalContext, title: &str) -> Self {
//         //         Task {
//         //             id: TaskId::new(),
//         //             title: {
//         //                 let mut val = DotFun::default();
//         //                 val.set(cc.ntitle.to_string());
//         //                 val
//         //             },
//         //             done: DotFun::default(),
//         //         }
//         //     }
//         // }
//         // pub fn upsert_task(
//         //     &mut self,
//         //     task: Task,
//         //     cc: &CausalContext,
//         // ) -> CausalDotStore<DotMap<TaskId, Task>> {
//         //     let mut ret_dot_map = CausalDotStore::default();
//         //     let existing_v = if let Some(existing_task) = self.store.store.get(&task.id) {
//         //         existing_task
//         //     } else {
//         //         &Task::new()
//         //     };

//         //     // // apply `O` to generate the new value for this key,
//         //     let CausalDotStore {
//         //         store: new_v,
//         //         context: ret_cc,
//         //     } = {
//         //       existing_v.join
//         //     };

//         //     // apply insert to the map
//         //     // let mut new_v = v.clone();
//         //     v.store.set(cc.dot, task.into());
//         //     let ret_cc = cc.join(&new_v.context);

//         //     ret_dot_map.store.set(task.id, new_v.into());

//         //     CausalDotStore {
//         //         store: ret_dot_map,
//         //         context: ret_cc,
//         //     }
//         // }

//         // pub fn update_task(&mut self, task_id: &TaskId, update: impl FnOnce(Task)) {
//         //     let mut_task: Task = self.store.store.get(task_id).cloned().unwrap_or_default();
//         //     update(mut_task);
//         // }
//         pub fn delta_add_task(&mut self, task_title: &str) -> Delta<DotMap<TaskId, Task>> {
//             let next_dot = self.store.context.next_dot_for(self.owner);

//             // create the delta data
//             let mut delta_data = DotMap::default();
//             let task = Task::new_at_dot(next_dot, task_title);
//             delta_data.set(task.id, task);

//             // then create the delta causal context
//             let mut delta_cc = CausalContext::new();
//             // Include old dots so they can be tombstoned/replaced
//             self.store.store.add_dots_to(&mut delta_cc);
//             // Include the new dot
//             delta_cc.insert_dot(next_dot);

//             Delta(CausalDotStore {
//                 store: delta_data,
//                 context: delta_cc,
//             })
//         }
//         pub fn mut_add_task(&mut self, title: &str) -> Result<Delta<DotMap<TaskId, Task>>, StrErr> {
//             let delta = self.delta_add_task(title);

//             // apply the delta to the store
//             self.store
//                 .clone()
//                 .join(delta.0.clone(), &mut Validator {})
//                 .context("")?;
//             Ok(delta)
//         }
//     }

//     #[cfg(test)]
//     mod tests {
//         use super::*;

//         #[test]
//         #[ignore]
//         fn test_mut_add_task() -> anyhow::Result<()> {
//             let alice = ActorId::new(0, 0);
//             let mut store_alice = TaskMapStore::new(alice);
//             let delta = store_alice.mut_add_task("test task")?;
//             // dbg!(&delta, &store_alice);
//             Ok(())
//         }

//         #[test]
//         fn test_concurrent_add_task() -> anyhow::Result<()> {
//             let alice = ActorId::new(0, 0);
//             let mut store_alice = TaskMapStore::new(alice);
//             let bob = ActorId::new(1, 0);
//             let mut store_bob = TaskMapStore::new(bob);

//             let delta_alice = store_alice.mut_add_task("alice task")?;
//             dbg!(&delta_alice, &store_alice);
//             let delta_bob = store_bob.mut_add_task("bob task")?;

//             // Sync: exchange deltas
//             let store_a = store_alice.join(&delta_bob);
//             dbg!(&store_a);

//             // TODO NEXT asserts for alice and bob stores

//             // store_bob.join(&delta_alice);
//             // dbg!(&store_bob);
//             //     .store
//             //     .join_or_replace_with(delta_bob.store.clone(), &delta_bob.context);
//             // store_bob
//             //     .store
//             //     .join_or_replace_with(delta_alice.store.clone(), &delta_alice.context);
//             // dbg!(&delta_alice, &delta_bob, &store_alice, &store_bob);
//             Ok(())
//         }
//     }
// }

pub mod task_map_2 {
    use crate::{ActorId, Delta, StrErr, Task, TaskId, TaskValidator};
    use dson::{CausalDotStore, DotFun, DotMap};

    #[derive(Debug)]
    pub struct TaskMap {
        pub tasks: CausalDotStore<DotMap<TaskId, Task>>,
        pub owner: ActorId,
    }
    impl TaskMap {
        pub fn new(owner: ActorId) -> Self {
            Self {
                tasks: CausalDotStore::default(),
                owner,
            }
        }
        pub fn join(self, other: TaskMapDelta) -> Result<Self, StrErr> {
            let joined = self.tasks.join(other.0, &mut TaskValidator)?;
            Ok(TaskMap {
                tasks: joined,
                owner: self.owner,
            })
        }
        pub fn add_task(&mut self, title: &str) -> (Task, TaskMapDelta) {
            let prev_cc = self.tasks.context.clone();
            let dot1_a = self.tasks.context.next_dot_for(self.owner);
            let mut t1 = Task {
                id: TaskId::new(),
                title: DotFun::default(),
                done: DotFun::default(),
            };
            t1.title.set(dot1_a, title.to_string());
            self.tasks.store.set(t1.id, t1.clone());
            self.tasks.context.insert_next_dot(dot1_a);

            let delta_inner = self.tasks.subset_for_inflation_from(&prev_cc);
            (t1, Delta(delta_inner))
        }
        pub fn set_task_title(&mut self, id: TaskId, title: &str) -> Result<TaskMapDelta, StrErr> {
            let prev_cc = self.tasks.context.clone();
            let dot1_a = self.tasks.context.next_dot_for(self.owner);
            let mut t1 = self
                .tasks
                .store
                .get_mut_and_invalidate(&id)
                .ok_or_else(|| StrErr {
                    message: "task not found".to_string(),
                })?
                .clone();
            t1.title.set(dot1_a, title.to_string());
            self.tasks.store.set(id, t1.clone());
            self.tasks.context.insert_next_dot(dot1_a);

            let delta_inner = self.tasks.subset_for_inflation_from(&prev_cc);
            Ok(Delta(delta_inner))
        }
    }

    pub type TaskMapDelta = Delta<DotMap<TaskId, Task>>;

    #[cfg(test)]
    mod tests {
        use super::*;
        use dson::DotStore;

        #[test]
        fn test_taskmap_concurrent_add_task() -> anyhow::Result<()> {
            let alice = ActorId::new(0, 0);
            let bob = ActorId::new(1, 0);
            let mut tm_a = TaskMap::new(alice);
            let mut tm_b = TaskMap::new(bob);

            let (t1a, delta_a) = tm_a.add_task("task_a1");
            let (t1b, delta_b) = tm_b.add_task("task_b1");

            let mut expected_context = tm_a.tasks.context.clone();
            expected_context.union(&tm_b.tasks.context);

            tm_a = tm_a.join(delta_b)?;
            tm_b = tm_b.join(delta_a)?;

            // let joined = tm_a.join(tm_b).unwrap();

            assert_eq!(tm_a.tasks.context, expected_context);
            assert!(!tm_a.tasks.store.is_bottom());
            assert_eq!(tm_a.tasks.store.get(&t1a.id), Some(&t1a));
            assert_eq!(tm_a.tasks.store.get(&t1b.id), Some(&t1b));
            assert_eq!(tm_b.tasks.context, expected_context);
            assert!(!tm_b.tasks.store.is_bottom());
            assert_eq!(tm_b.tasks.store.get(&t1a.id), Some(&t1a));
            assert_eq!(tm_b.tasks.store.get(&t1b.id), Some(&t1b));
            assert_eq!(tm_b.tasks, tm_a.tasks);

            Ok(())
        }

        #[test]
        fn test_concurrent_set_task_title() -> anyhow::Result<()> {
            let alice = ActorId::new(0, 0);
            let bob = ActorId::new(1, 0);
            let mut tm_a = TaskMap::new(alice);
            let mut tm_b = TaskMap::new(bob);

            // create task and sync with bob
            let (t1a, delta_a) = tm_a.add_task("task_a1");
            tm_b = tm_b.join(delta_a)?;

            // set task title concurrently and sync
            let delta_a = tm_a.set_task_title(t1a.id, "A did this")?;
            let delta_b = tm_b.set_task_title(t1a.id, "B did this")?;
            tm_a = tm_a.join(delta_b)?;
            tm_b = tm_b.join(delta_a)?;

            dbg!(&tm_a, &tm_b);

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{ActorId, Task, TaskId, TaskValidator};
    use dson::{CausalDotStore, DotFun, DotMap, DotStore};

    #[test]
    fn test_dotmap_concurrent_add_task() -> anyhow::Result<()> {
        let alice = ActorId::new(0, 0);
        let bob = ActorId::new(1, 0);
        let mut ds1 = CausalDotStore::<DotMap<TaskId, Task>>::default();
        let mut ds2 = CausalDotStore::<DotMap<TaskId, Task>>::default();

        let dot1_a = ds1.context.next_dot_for(alice);
        let mut t1 = Task {
            id: TaskId::new(),
            title: DotFun::default(),
            done: DotFun::default(),
        };
        t1.title.set(dot1_a, "dot1_a".to_string());
        ds1.store.set(t1.id, t1.clone());
        ds1.context.insert_next_dot(dot1_a);

        let dot2_b = ds2.context.next_dot_for(bob);
        let mut t2 = Task {
            id: TaskId::new(),
            title: DotFun::default(),
            done: DotFun::default(),
        };
        t2.title.set(dot2_b, "dot2_b".to_string());
        ds2.store.set(t2.id, t2.clone());
        ds2.context.insert_next_dot(dot2_b);

        let mut expected_context = ds1.context.clone();
        expected_context.union(&ds2.context);

        let join = ds1.join(ds2, &mut TaskValidator).unwrap();

        assert_eq!(join.context, expected_context);
        assert!(!join.store.is_bottom());
        assert_eq!(join.store.get(&t1.id), Some(&t1));
        assert_eq!(join.store.get(&t2.id), Some(&t2));

        Ok(())
    }
}
