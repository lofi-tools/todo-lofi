pub mod task {
    use dson::{
        CausalContext, CausalDotStore, Dot, DotChange, DotFun, DotStore, DotStoreJoin,
        DryJoinOutput, Identifier,
    };
    use entity_id::EntityId;
    use ulid::Ulid;

    #[derive(EntityId, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
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
    /// We use DotFun directly to represent the fields.
    /// DotFun maps a Dot (unique event ID) to a Value.
    #[derive(Debug, Clone, PartialEq)]
    pub struct Task {
        pub id: TaskId,
        /// The title of the task. Uses DotFun<String>.
        /// Can contain multiple values during a conflict, which we resolve in `join`.
        pub title: DotFun<String>,
        /// The done status. Uses DotFun<bool>.
        pub done: DotFun<bool>,
    }
    impl Default for Task {
        fn default() -> Self {
            Self {
                id: TaskId(Ulid::default()),
                title: DotFun::default(),
                done: DotFun::default(),
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
    impl DotStore for Task {
        /// Collect all dots used by this Task (from both fields).
        fn add_dots_to(&self, other: &mut CausalContext) {
            self.title.add_dots_to(other);
            self.done.add_dots_to(other);
        }

        /// Check if the task is empty (no title and no done status).
        fn is_bottom(&self) -> bool {
            self.title.is_bottom() && self.done.is_bottom()
        }

        /// Create a subset of the store for inflation/delta computation.
        fn subset_for_inflation_from(&self, frontier: &CausalContext) -> Self {
            Task {
                id: self.id,
                title: self.title.subset_for_inflation_from(frontier),
                done: self.done.subset_for_inflation_from(frontier),
            }
        }
    }

    impl<S> DotStoreJoin<S> for Task
    where
        S: dson::sentinel::Sentinel
            + dson::sentinel::ValueSentinel<String>
            + dson::sentinel::ValueSentinel<bool>,
    {
        fn join(
            (s1, c1): (Self, &CausalContext),
            (s2, c2): (Self, &CausalContext),
            on_dot_change: &mut dyn FnMut(DotChange),
            sentinel: &mut S,
        ) -> Result<Self, S::Error> {
            if s1.id != s2.id {
                return Ok(s1); // Should be error: cannot join tasks with different ids
            }

            // Join field field
            let mut new_title =
                DotStoreJoin::join((s1.title, c1), (s2.title, c2), on_dot_change, sentinel)?;
            let mut new_done =
                DotStoreJoin::join((s1.done, c1), (s2.done, c2), on_dot_change, sentinel)?;

            // Conflict Resolution (LWW - Last Writer Wins)
            // If concurrent writes caused multiple values (dots) to exist in a field,
            // we pick the one with the "maximum" dot (highest seq, then highest ID).
            // This ensures the field acts as a "Single-Value Register".
            if new_title.len() > 1
                && let Some((dot, val)) = new_title.iter().max_by_key(|(d, _)| *d)
            {
                let winner = val.clone();
                let winner_dot = dot;
                new_title = DotFun::default();
                new_title.set(winner_dot, winner);
            }

            if new_done.len() > 1
                && let Some((dot, val)) = new_done.iter().max_by_key(|(d, _)| *d)
            {
                let winner = *val;
                let winner_dot = dot;
                new_done = DotFun::default();
                new_done.set(winner_dot, winner);
            }

            Ok(Task {
                id: s1.id,
                title: new_title,
                done: new_done,
            })
        }

        fn dry_join(
            (s1, c1): (&Self, &CausalContext),
            (s2, c2): (&Self, &CausalContext),
            sentinel: &mut S,
        ) -> Result<DryJoinOutput, S::Error> {
            // Simplified dry join: check if any field would change.
            let title_out = DotStoreJoin::dry_join((&s1.title, c1), (&s2.title, c2), sentinel)?;
            let done_out = DotStoreJoin::dry_join((&s1.done, c1), (&s2.done, c2), sentinel)?;

            // If either field changes, the struct changes.
            Ok(DryJoinOutput::new(
                title_out.is_bottom() && done_out.is_bottom(),
            ))
        }
    }

    impl Task {
        /// Creates a delta to set the title.
        pub fn delta_set_title(
            &self,
            value: &str,
            cc: &CausalContext,
            id: Identifier,
        ) -> Delta<Self> {
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
}

pub mod single_task {
    use crate::task::{ActorId, Delta, Task};
    use dson::{CausalDotStore, Identifier};

    #[derive(Debug, PartialEq)]
    pub struct SingleTaskStore {
        store: CausalDotStore<Task>,
        owner: ActorId,
    }
    impl SingleTaskStore {
        pub fn new(owner: Identifier) -> Self {
            Self {
                store: CausalDotStore::default(),
                owner,
            }
        }
        pub fn mut_set_title(&mut self, title: &str) -> Delta<Task> {
            let title_delta =
                Task::delta_set_title(&self.store.store, title, &self.store.context, self.owner);

            // apply the delta to the store
            self.store
                .join_or_replace_with(title_delta.store.clone(), &title_delta.context);
            title_delta
        }
        pub fn get_title(&self) -> Option<&String> {
            self.store.store.get_title()
        }
        pub fn join(&mut self, other: Delta<Task>) {
            self.store
                .join_or_replace_with(other.0.store, &other.0.context);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // #[test]
        // fn test_set_title() {
        //     let alice = Identifier::new(0, 0);
        //     let mut store_alice = SingleTaskStore::new(alice);
        //     store_alice.mut_set_title("Test Task");
        //     assert_eq!(
        //         store_alice.get_title().cloned(),
        //         Some("Test Task".to_string())
        //     );
        // }

        #[test]
        fn test_concurrent_set_title() {
            let alice = Identifier::new(0, 0);
            let bob = Identifier::new(1, 0);

            let mut store_alice = SingleTaskStore::new(alice);
            // deltas are applied to the store in set_title()
            let delta1_alice = store_alice.mut_set_title("Alice title");
            assert_eq!(
                store_alice.get_title().cloned(),
                Some("Alice title".to_string())
            );

            let mut store_bob = SingleTaskStore::new(bob);
            let delta1_bob = store_bob.mut_set_title("Bob title");
            assert_eq!(
                store_bob.get_title().cloned(),
                Some("Bob title".to_string())
            );

            // Sync: exchange deltas
            store_alice.join(delta1_bob);
            store_bob.join(delta1_alice);
            // dbg!(&store_alice, &store_bob);
            assert_eq!(store_alice.store.store, store_bob.store.store);

            let delta2_bob = store_bob.mut_set_title("Bob change title again");
            store_alice.join(delta2_bob);
            assert_eq!(
                store_alice.get_title().cloned(),
                Some("Bob change title again".to_string())
            );
            assert_eq!(
                store_bob.get_title().cloned(),
                Some("Bob change title again".to_string())
            );
        }

        // #[test]
        // fn test_set_done() {
        //     let id = Identifier::new(0, 0);
        //     let mut task = Task::default();
        //     let current_ctx = CausalContext::default();
        //     let done_delta = task.set_done(true, &current_ctx, id);
        //     assert_eq!(done_delta.store.done.get(&Dot::new(id, 1)), Some(&true));
        //     assert_eq!(done_delta.context.len(), 1);
        // }
    }
}

pub mod task_map {
    use std::convert::Infallible;

    use crate::task::{ActorId, Delta, Task, TaskId};
    use dson::{
        CausalContext, CausalDotStore, DotChange, DotMap, DotStore, DotStoreJoin, Identifier,
        sentinel::DummySentinel,
    };

    pub struct TaskMap(pub DotMap<TaskId, Task>);
    impl std::ops::Deref for TaskMap {
        type Target = DotMap<TaskId, Task>;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
    impl std::ops::DerefMut for TaskMap {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }
    impl TaskMap {
        pub fn delta_add_task(
            &mut self,
            task_title: &str,
            cc: &CausalContext,
            id: Identifier,
        ) -> Delta<Self> {
            let next_dot = cc.next_dot_for(id);

            // create the delta data
            let mut delta_data = DotMap::default();
            let task = Task::new_at_dot(next_dot, task_title);
            delta_data.set(task.id, task);

            // then create the delta causal context
            let mut delta_cc = CausalContext::new();
            // Include old dots so they can be tombstoned/replaced
            // self.0.add_dots_to(&mut delta_cc);
            // Include the new dot
            delta_cc.insert_dot(next_dot);

            Delta(CausalDotStore {
                store: TaskMap(delta_data),
                context: delta_cc,
            })
        }
    }

    #[derive(Debug, Clone)]
    pub struct TaskMapStore {
        store: CausalDotStore<DotMap<TaskId, Task>>,
        owner: ActorId,
    }
    impl TaskMapStore {
        pub fn new(owner: Identifier) -> Self {
            Self {
                store: CausalDotStore::default(),
                owner,
            }
        }
        pub fn join(
            self,
            other: &Delta<DotMap<TaskId, Task>>,
        ) -> Result<DotMap<TaskId, Task>, Infallible> {
            dbg!(&self, &other);
            // self.store
            //     .join_or_replace_with(other.store.clone(), &other.context)
            DotStoreJoin::join(
                (self.store.store, &self.store.context),
                (other.store.clone(), &other.context),
                &mut |_change| {},
                &mut DummySentinel,
            )
        }
        // pub fn map(&self) -> &DotMap<TaskId, Task> {
        //     &self.store.store
        // }
        // pub fn context(&mut self) -> &mut CausalContext {
        //     &mut self.store.context
        // }

        // pub fn new_task(&mut self, title: &str) -> Delta<DotMap<TaskId, Task>> {
        //     pub fn new(cc: &CausalContext, title: &str) -> Self {
        //         Task {
        //             id: TaskId::new(),
        //             title: {
        //                 let mut val = DotFun::default();
        //                 val.set(cc.ntitle.to_string());
        //                 val
        //             },
        //             done: DotFun::default(),
        //         }
        //     }
        // }
        // pub fn upsert_task(
        //     &mut self,
        //     task: Task,
        //     cc: &CausalContext,
        // ) -> CausalDotStore<DotMap<TaskId, Task>> {
        //     let mut ret_dot_map = CausalDotStore::default();
        //     let existing_v = if let Some(existing_task) = self.store.store.get(&task.id) {
        //         existing_task
        //     } else {
        //         &Task::new()
        //     };

        //     // // apply `O` to generate the new value for this key,
        //     let CausalDotStore {
        //         store: new_v,
        //         context: ret_cc,
        //     } = {
        //       existing_v.join
        //     };

        //     // apply insert to the map
        //     // let mut new_v = v.clone();
        //     v.store.set(cc.dot, task.into());
        //     let ret_cc = cc.join(&new_v.context);

        //     ret_dot_map.store.set(task.id, new_v.into());

        //     CausalDotStore {
        //         store: ret_dot_map,
        //         context: ret_cc,
        //     }
        // }

        // pub fn update_task(&mut self, task_id: &TaskId, update: impl FnOnce(Task)) {
        //     let mut_task: Task = self.store.store.get(task_id).cloned().unwrap_or_default();
        //     update(mut_task);
        // }
        pub fn delta_add_task(&mut self, task_title: &str) -> Delta<DotMap<TaskId, Task>> {
            let next_dot = self.store.context.next_dot_for(self.owner);

            // create the delta data
            let mut delta_data = DotMap::default();
            let task = Task::new_at_dot(next_dot, task_title);
            delta_data.set(task.id, task);

            // then create the delta causal context
            let mut delta_cc = CausalContext::new();
            // Include old dots so they can be tombstoned/replaced
            self.store.store.add_dots_to(&mut delta_cc);
            // Include the new dot
            delta_cc.insert_dot(next_dot);

            Delta(CausalDotStore {
                store: delta_data,
                context: delta_cc,
            })
        }
        pub fn mut_add_task(&mut self, title: &str) -> Delta<DotMap<TaskId, Task>> {
            let delta = self.delta_add_task(title);

            // apply the delta to the store
            self.store
                .join_or_replace_with(delta.store.clone(), &delta.context);
            delta
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        #[ignore]
        fn test_mut_add_task() {
            let alice = ActorId::new(0, 0);
            let mut store_alice = TaskMapStore::new(alice);
            let delta = store_alice.mut_add_task("test task");
            // dbg!(&delta, &store_alice);
        }

        #[test]
        fn test_concurrent_add_task() {
            let alice = ActorId::new(0, 0);
            let mut store_alice = TaskMapStore::new(alice);
            let bob = ActorId::new(1, 0);
            let mut store_bob = TaskMapStore::new(bob);

            let delta_alice = store_alice.mut_add_task("alice task");
            let delta_bob = store_bob.mut_add_task("bob task");

            // Sync: exchange deltas
            let store_a = store_alice.join(&delta_bob);
            dbg!(&store_a);
            // store_bob.join(&delta_alice);
            // dbg!(&store_bob);
            //     .store
            //     .join_or_replace_with(delta_bob.store.clone(), &delta_bob.context);
            // store_bob
            //     .store
            //     .join_or_replace_with(delta_alice.store.clone(), &delta_alice.context);
            // dbg!(&delta_alice, &delta_bob, &store_alice, &store_bob);
        }
    }
}

#[cfg(test)]
mod dotmap_join_tests {
    use dson::{
        CausalContext, CausalDotStore, DotChange, DotFun, DotMap, DotStore, DotStoreJoin,
        DryJoinOutput, Identifier,
        sentinel::{DummySentinel, Visit},
    };

    #[test]
    // reproduce library test
    fn test_dotmap_dotfun_join_disjoint_keys() {
        // let mut validator = KeyCountingValidator::default();
        let alice = Identifier::new(0, 0);
        let bob = Identifier::new(1, 0);

        let mut ds1 = CausalDotStore::<DotMap<_, DotFun<_>>>::default();
        let mut ds2 = CausalDotStore::<DotMap<_, DotFun<_>>>::default();

        let key1 = "foo";
        let dot1_a = ds1.context.next_dot_for(alice);
        let mut v1 = DotFun::default();
        v1.set(dot1_a, "dot1_a");
        ds1.store.set(key1, v1.clone());
        ds1.context.insert_next_dot(dot1_a);

        let key2 = "bar";
        let dot2_b = ds2.context.next_dot_for(bob);
        let mut v2 = DotFun::default();
        v2.set(dot2_b, "dot2_b");
        ds2.store.set(key2, v2.clone());
        ds2.context.insert_next_dot(dot2_b);

        let mut expected_context = ds1.context.clone();
        expected_context.union(&ds2.context);

        let join = ds1
            .join(
                ds2,
                // &mut |change| assert_eq!(change, DotChange::Add(dot2)),
                &mut DummySentinel,
            )
            .unwrap();
        // assert_eq!(validator.added, 1);
        // assert_eq!(validator.removed, 0);

        assert_eq!(join.context, expected_context);
        assert!(!join.store.is_bottom());
        assert_eq!(join.store.get(key1), Some(&v1));
        assert_eq!(join.store.get(key2), Some(&v2));
    }

    #[test]
    // wrap DotFun as DotStore struct field
    fn test_dotmap_wrap_dotfun_join_disjoint_keys() {
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct MyStore {
            field: DotFun<String>,
        }
        impl DotStore for MyStore {
            fn add_dots_to(&self, other: &mut CausalContext) {
                self.field.add_dots_to(other);
            }
            fn is_bottom(&self) -> bool {
                self.field.is_bottom()
            }
            fn subset_for_inflation_from(&self, frontier: &CausalContext) -> Self {
                Self {
                    field: self.field.subset_for_inflation_from(frontier),
                }
            }
        }
        impl<S> DotStoreJoin<S> for MyStore
        where
            S: dson::sentinel::Sentinel + dson::sentinel::ValueSentinel<String>,
        {
            fn join(
                (ds1, c1): (Self, &CausalContext),
                (ds2, c2): (Self, &CausalContext),
                on_dot_change: &mut dyn FnMut(DotChange),
                sentinel: &mut S,
            ) -> Result<Self, S::Error> {
                Ok(Self {
                    field: DotStoreJoin::join(
                        (ds1.field, c1),
                        (ds2.field, c2),
                        on_dot_change,
                        sentinel,
                    )?,
                })
            }
            fn dry_join(
                (s1, c1): (&Self, &CausalContext),
                (s2, c2): (&Self, &CausalContext),
                sentinel: &mut S,
            ) -> Result<DryJoinOutput, S::Error> {
                todo!()
            }
        }

        let alice = Identifier::new(0, 0);
        let bob = Identifier::new(1, 0);
        let mut ds1 = CausalDotStore::<DotMap<_, MyStore>>::default();
        let mut ds2 = CausalDotStore::<DotMap<_, MyStore>>::default();

        let key1 = "foo";
        let dot1_a = ds1.context.next_dot_for(alice);
        let mut v1 = MyStore {
            field: DotFun::default(),
        };
        v1.field.set(dot1_a, "dot1_a".to_string());
        ds1.store.set(key1, v1.clone());
        ds1.context.insert_next_dot(dot1_a);

        let key2 = "bar";
        let dot2_b = ds2.context.next_dot_for(bob);
        let mut v2 = MyStore {
            field: DotFun::default(),
        };
        v2.field.set(dot2_b, "dot2_b".to_string());
        ds2.store.set(key2, v2.clone());
        ds2.context.insert_next_dot(dot2_b);

        let mut expected_context = ds1.context.clone();
        expected_context.union(&ds2.context);

        let join = ds1.join(ds2, &mut DummySentinel).unwrap();

        assert_eq!(join.context, expected_context);
        assert!(!join.store.is_bottom());
        assert_eq!(join.store.get(key1), Some(&v1));
        assert_eq!(join.store.get(key2), Some(&v2));
    }

    #[test]
    // add an ID to MyStore
    fn test_dotmap_struct_join_disjoint_keys() {
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct MyStore {
            id: String,
            title: DotFun<String>,
        }
        impl DotStore for MyStore {
            fn add_dots_to(&self, other: &mut CausalContext) {
                self.title.add_dots_to(other);
            }
            fn is_bottom(&self) -> bool {
                self.title.is_bottom()
            }
            fn subset_for_inflation_from(&self, frontier: &CausalContext) -> Self {
                Self {
                    id: self.id.clone(),
                    title: self.title.subset_for_inflation_from(frontier),
                }
            }
        }

        pub struct Validator;
        impl dson::sentinel::Sentinel for Validator {
            type Error = String;
        }
        impl dson::sentinel::ValueSentinel<String> for Validator {}
        impl dson::sentinel::KeySentinel for Validator {}
        impl<K> dson::sentinel::Visit<K> for Validator {
            fn enter(&mut self, key: &K) -> Result<(), Self::Error> {
                Ok(())
            }
            fn exit(&mut self) -> Result<(), Self::Error> {
                Ok(())
            }
        }

        impl DotStoreJoin<Validator> for MyStore {
            fn join(
                (ds1, c1): (Self, &CausalContext),
                (ds2, c2): (Self, &CausalContext),
                on_dot_change: &mut dyn FnMut(DotChange),
                sentinel: &mut Validator,
            ) -> Result<Self, String> {
                if ds1 == Self::default() {
                    return Ok(ds2);
                }
                if ds2 == Self::default() {
                    return Ok(ds1);
                }
                if ds1.id != String::default() && ds2.id != String::default() && ds1.id != ds2.id {
                    return Err(format!("ID mismatch: \"{}\" != \"{}\"", ds1.id, ds2.id));
                }
                Ok(Self {
                    id: ds1.id.clone(),
                    title: DotStoreJoin::join(
                        (ds1.title, c1),
                        (ds2.title, c2),
                        on_dot_change,
                        sentinel,
                    )?,
                })
            }
            fn dry_join(
                (s1, c1): (&Self, &CausalContext),
                (s2, c2): (&Self, &CausalContext),
                sentinel: &mut Validator,
            ) -> Result<DryJoinOutput, String> {
                todo!()
            }
        }

        let alice = Identifier::new(0, 0);
        let bob = Identifier::new(1, 0);
        let mut ds1 = CausalDotStore::<DotMap<_, MyStore>>::default();
        let mut ds2 = CausalDotStore::<DotMap<_, MyStore>>::default();

        let dot1_a = ds1.context.next_dot_for(alice);
        let mut v1 = MyStore {
            id: "foo".to_string(),
            title: DotFun::default(),
        };
        v1.title.set(dot1_a, "dot1_a".to_string());
        ds1.store.set(&v1.id, v1.clone());
        ds1.context.insert_next_dot(dot1_a);

        let dot2_b = ds2.context.next_dot_for(bob);
        let mut v2 = MyStore {
            id: "bar".to_string(),
            title: DotFun::default(),
        };
        v2.title.set(dot2_b, "dot2_b".to_string());
        ds2.store.set(&v2.id, v2.clone());
        ds2.context.insert_next_dot(dot2_b);

        let mut expected_context = ds1.context.clone();
        expected_context.union(&ds2.context);

        let join = ds1.join(ds2, &mut Validator).unwrap();
        dbg!(&join);

        assert_eq!(join.context, expected_context);
        assert!(!join.store.is_bottom());
        assert_eq!(join.store.get(&v1.id), Some(&v1));
        assert_eq!(join.store.get(&v2.id), Some(&v2));
    }

    #[test]
    // add fields to MyStore
    fn test_dotmap_task_join_disjoint_keys() {
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct Task {
            id: String,
            title: DotFun<String>,
            done: DotFun<bool>,
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
                    id: self.id.clone(),
                    title: self.title.subset_for_inflation_from(frontier),
                    done: self.done.subset_for_inflation_from(frontier),
                }
            }
        }

        pub struct Validator;
        impl dson::sentinel::Sentinel for Validator {
            type Error = String;
        }
        impl<K> dson::sentinel::ValueSentinel<K> for Validator {}
        impl dson::sentinel::KeySentinel for Validator {}
        impl<K> dson::sentinel::Visit<K> for Validator {
            fn enter(&mut self, _key: &K) -> Result<(), Self::Error> {
                Ok(())
            }
            fn exit(&mut self) -> Result<(), Self::Error> {
                Ok(())
            }
        }

        impl DotStoreJoin<Validator> for Task {
            fn join(
                (ds1, c1): (Self, &CausalContext),
                (ds2, c2): (Self, &CausalContext),
                on_dot_change: &mut dyn FnMut(DotChange),
                sentinel: &mut Validator,
            ) -> Result<Self, String> {
                if ds1 == Self::default() {
                    return Ok(ds2);
                }
                if ds2 == Self::default() {
                    return Ok(ds1);
                }
                if ds1.id != String::default() && ds2.id != String::default() && ds1.id != ds2.id {
                    return Err(format!("ID mismatch: \"{}\" != \"{}\"", ds1.id, ds2.id));
                }
                Ok(Self {
                    id: ds1.id.clone(),
                    title: DotStoreJoin::join(
                        (ds1.title, c1),
                        (ds2.title, c2),
                        on_dot_change,
                        sentinel,
                    )?,
                    done: DotStoreJoin::join(
                        (ds1.done, c1),
                        (ds2.done, c2),
                        on_dot_change,
                        sentinel,
                    )?,
                })
            }
            fn dry_join(
                (s1, c1): (&Self, &CausalContext),
                (s2, c2): (&Self, &CausalContext),
                sentinel: &mut Validator,
            ) -> Result<DryJoinOutput, String> {
                todo!()
            }
        }

        let alice = Identifier::new(0, 0);
        let bob = Identifier::new(1, 0);
        let mut ds1 = CausalDotStore::<DotMap<_, Task>>::default();
        let mut ds2 = CausalDotStore::<DotMap<_, Task>>::default();

        let dot1_a = ds1.context.next_dot_for(alice);
        let mut v1 = Task {
            id: "foo".to_string(),
            title: DotFun::default(),
            done: DotFun::default(),
        };
        v1.title.set(dot1_a, "dot1_a".to_string());
        ds1.store.set(&v1.id, v1.clone());
        ds1.context.insert_next_dot(dot1_a);

        let dot2_b = ds2.context.next_dot_for(bob);
        let mut v2 = Task {
            id: "bar".to_string(),
            title: DotFun::default(),
            done: DotFun::default(),
        };
        v2.title.set(dot2_b, "dot2_b".to_string());
        ds2.store.set(&v2.id, v2.clone());
        ds2.context.insert_next_dot(dot2_b);

        let mut expected_context = ds1.context.clone();
        expected_context.union(&ds2.context);

        let join = ds1.join(ds2, &mut Validator).unwrap();

        assert_eq!(join.context, expected_context);
        assert!(!join.store.is_bottom());
        assert_eq!(join.store.get(&v1.id), Some(&v1));
        assert_eq!(join.store.get(&v2.id), Some(&v2));
    }

    // {
    //     // Setup: two independent maps with disjoint keys
    //     let alice = Identifier::new(0, 0);
    //     let bob = Identifier::new(1, 0);

    //     let mut store_a: CausalDotStore<DotMap<TaskId, Task>> = CausalDotStore::default();
    //     let mut store_b: CausalDotStore<DotMap<TaskId, Task>> = CausalDotStore::default();

    //     let next_dot_alice = store_a.context.next_dot_for(alice);
    //     let task_a1 = Task::new_at_dot(next_dot_alice, "task_a1");
    //     store_a.store.set(task_a1.id, task_a1.clone());
    //     store_a.context.insert_next_dot(next_dot_alice);

    //     // let next_dot_alice = store_a.context.next_dot_for(alice);
    //     // let task_a2 = Task::new_at_dot(next_dot_alice, "task_a2");
    //     // store_a.store.set(task_a2.id, task_a2);
    //     // store_a.context.insert_next_dot(next_dot_alice);

    //     let next_dot_bob = store_b.context.next_dot_for(bob);
    //     // let  task_a = DotFun::default();
    //     let task_b1 = Task::new_at_dot(next_dot_bob, "task_b1");
    //     // task_a.set(next_dot_alice, "alice task".to_string());
    //     store_b.store.set(task_b1.id, task_b1.clone());
    //     store_b.context.insert_next_dot(next_dot_bob);

    //     dbg!(next_dot_alice, next_dot_bob);

    //     // Sync: exchange deltas
    //     // store_a.join_or_replace_with(store_b.store, &store_b.context);
    //     let joined = store_a
    //         .join(
    //             store_b,
    //             // &mut |change| assert_eq!(change, DotChange::Add(dot2)),
    //             &mut DummySentinel,
    //         )
    //         .unwrap();

    //     dbg!(&joined);
    //     // let next_dot_bob = store_b.context.next_dot_for(bob);

    //     // // Create tasks at distinct dots
    //     // let task_a = Task::new_at_dot(next_dot_alice, "task_alice_1");
    //     // let task_b = Task::new_at_dot(next_dot_bob, "task_bob_1");

    //     // map_a.set(task_a.id, task_a);
    //     // map_b.set(task_b.id, task_b);

    //     // Directly test DotMap merge (bypassing CausalDotStore)
    //     // let mut result = map_a.clone();
    //     // let joined = DotStoreJoin::join((map_a, &cc_alice), (map_b, &cc_bob));
    //     // map_a.join(&map_b); // ← Key test: does this preserve both entries?

    //     // assert!(result.contains_key(&id_a), "Alice's task should be present");
    //     // assert!(result.contains_key(&id_b), "Bob's task should be present");
    //     // assert_eq!(result.len(), 2, "Should have both tasks");
    // }
    // }

    // #[test]
    // fn test_causal_dot_store_join_disjoint() {
    //     // Test the full CausalDotStore join with minimal, correct contexts
    //     let actor_a = Identifier::new(0, 0);
    //     let actor_b = Identifier::new(1, 0);

    //     let id_a = TaskId::new();
    //     let id_b = TaskId::new();

    //     // Store A: one task, context {@0.0: 1}
    //     let mut store_a_map = DotMap::default();
    //     store_a_map.set(id_a, Task::new_at_dot((actor_a, 1), "alice"));
    //     let mut cc_a = CausalContext::new();
    //     cc_a.insert_dot((actor_a, 1));
    //     let cds_a = CausalDotStore {
    //         store: store_a_map,
    //         context: cc_a,
    //     };

    //     // Store B: disjoint task, context {@1.0: 1}
    //     let mut store_b_map = DotMap::default();
    //     store_b_map.set(id_b, Task::new_at_dot((actor_b, 1), "bob"));
    //     let mut cc_b = CausalContext::new();
    //     cc_b.insert_dot((actor_b, 1));
    //     let cds_b = CausalDotStore {
    //         store: store_b_map,
    //         context: cc_b,
    //     };

    //     // Merge B into A
    //     let mut result = cds_a.clone();
    //     result.join_or_replace_with(cds_b.store, &cds_b.context);

    //     assert!(result.store.contains_key(&id_a), "Alice's task missing");
    //     assert!(result.store.contains_key(&id_b), "Bob's task missing");
    //     assert_eq!(result.store.len(), 2);
    //     // Context should reflect both actors
    //     assert!(result.context.contains_dot((actor_a, 1)));
    //     assert!(result.context.contains_dot((actor_b, 1)));
    // }
}
