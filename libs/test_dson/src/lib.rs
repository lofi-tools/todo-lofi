#![cfg(test)]
#![allow(non_snake_case)]
use dson::crdts::ValueType;
use dson::sentinel::{KeySentinel, Sentinel, TypeSentinel, ValueSentinel, Visit};
use dson::{CausalContext, CausalDotStore, Dot, DotChange, DotFun, DotStore, DotStoreJoin};
use dson::{DotFunMap, sentinel::DummySentinel};
use std::collections::{BTreeMap, HashSet};
use std::convert::Infallible;
use std::fmt::Debug;
use std::num::NonZeroU64;

const SEQ_1: NonZeroU64 = NonZeroU64::MIN;
// const SEQ_2: NonZeroU64 = NonZeroU64::MIN.saturating_add(1);

mod test_dotfun {
    use super::*;

    #[test]
    fn join_drops_removed() {
        let mut validator = ValueCountingValidator::default();

        let mut ds1 = CausalDotStore::<DotFun<_>>::default();
        let mut ds2 = CausalDotStore::<DotFun<_>>::default();

        let dot1 = Dot::from(((0, 0), SEQ_1));
        ds1.store.set(dot1, "dot1");
        ds1.context.insert_next_dot(dot1);
        let dot2 = Dot::from(((1, 0), SEQ_1));
        ds2.store.set(dot2, "dot2");
        ds2.context.insert_next_dot(dot2);

        // make it so that ds1 has seen dot2, but does not have a value for it,
        // which implies that ds1 explicitly removed dot2.
        ds1.context.insert_next_dot(dot2);

        let mut expected_context = ds1.context.clone();
        expected_context.union(&ds2.context);

        // also check that the join is symmetrical
        // (modulo the semantics of on_dot_change and sentinels)
        let join = ds1
            .test_join(
                &ds2,
                &mut |_| unreachable!("ds1 has seen all of ds2, so no dot changes"),
                &mut validator,
            )
            .unwrap();

        assert_eq!(join.context, expected_context);
        assert!(!join.store.is_bottom());
        assert_eq!(join.store.get(&dot1).copied(), Some("dot1"));
        assert_eq!(join.store.get(&dot2), None);

        let mut added = HashSet::new();
        let mut removed = HashSet::new();
        let join = ds2
            .test_join(
                &ds1,
                &mut |change| match change {
                    DotChange::Add(d) if d == dot1 => {
                        assert!(added.insert(d), "on_dot_change added {d:?} twice");
                    }
                    DotChange::Remove(d) if d == dot2 => {
                        assert!(removed.insert(d), "on_dot_change removed {d:?} twice");
                    }
                    diff => unreachable!("{diff:?}"),
                },
                &mut validator,
            )
            .unwrap();

        assert_eq!(join.context, expected_context);
        assert!(!join.store.is_bottom());
        assert_eq!(join.store.get(&dot1).copied(), Some("dot1"));
        assert_eq!(join.store.get(&dot2), None);

        assert_eq!(added, HashSet::from_iter([dot1]));
        assert_eq!(removed, HashSet::from_iter([dot2]));

        assert_eq!(validator.added, BTreeMap::from([("dot1", 1)]));
        assert_eq!(validator.removed, BTreeMap::from([("dot2", 1)]));
    }
}

mod test_dotfunmap {
    use super::*;

    #[test]
    fn join_drops_removed() {
        let mut ds1 = CausalDotStore::<DotFunMap<DotFun<_>>>::default();
        let mut ds2 = CausalDotStore::<DotFunMap<DotFun<_>>>::default();

        let key1 = Dot::from(((9, 0), SEQ_1));
        let dot1 = Dot::from(((0, 0), SEQ_1));
        let mut v1 = DotFun::default();
        v1.set(dot1, "dot1");
        ds1.store.set(key1, v1.clone());
        ds1.context.insert_next_dot(dot1);
        let key2 = Dot::from(((8, 0), SEQ_1));
        let dot2 = Dot::from(((1, 0), SEQ_1));
        let mut v2 = DotFun::default();
        v2.set(dot2, "dot2");
        ds2.store.set(key2, v2.clone());
        ds2.context.insert_next_dot(dot2);

        // make it so that ds1 has seen dot2, but does not have a value for key1,
        // which implies that ds1 explicitly removed key1.
        ds1.context.insert_next_dot(key2);
        ds1.context.insert_next_dot(dot2);

        let mut expected_context = ds1.context.clone();
        expected_context.union(&ds2.context);

        // also check that the join is symmetrical
        // (modulo the semantics of on_dot_change and sentinels)
        let join = ds1
            .test_join(
                &ds2,
                &mut |_| unreachable!("ds1 has seen all of ds2, so no dot changes"),
                &mut DummySentinel,
            )
            .unwrap();

        assert_eq!(join.context, expected_context);
        assert!(!join.store.is_bottom());
        assert_eq!(join.store.get(&key1), Some(&v1));
        assert_eq!(join.store.get(&key2), None);

        let mut added = HashSet::new();
        let mut removed = HashSet::new();
        let join = ds2
            .test_join(
                &ds1,
                &mut |change| match change {
                    DotChange::Add(d) if [dot1, key1].contains(&d) => {
                        assert!(added.insert(d), "on_dot_change added {d:?} twice");
                    }
                    DotChange::Remove(d) if [dot2, key2].contains(&d) => {
                        assert!(removed.insert(d), "on_dot_change removed {d:?} twice");
                    }
                    diff => unreachable!("{diff:?}"),
                },
                &mut DummySentinel,
            )
            .unwrap();

        assert_eq!(join.context, expected_context);
        assert!(!join.store.is_bottom());
        assert_eq!(join.store.get(&key1), Some(&v1));
        assert_eq!(join.store.get(&key2), None);
        assert_eq!(added, HashSet::from_iter([dot1, key1]));
        assert_eq!(removed, HashSet::from_iter([dot2, key2]));
    }
}

// use super::*;
// use dson::crdts::ValueType;
// use dson::sentinel::{KeySentinel, Sentinel, TypeSentinel, ValueSentinel, Visit};
// use std::{convert::Infallible, fmt::Debug};

/// A sentinel that records all calls.
#[derive(Default)]
pub struct RecordingSentinel {
    path: Vec<String>,
    /// A string-representation of each call that the sentinel has received.
    /// This is mostly useful for tests.
    pub changes_seen: Vec<String>,
}
impl RecordingSentinel {
    /// Create a new PeekingSentinel
    pub fn new() -> RecordingSentinel {
        RecordingSentinel {
            path: vec![],
            changes_seen: vec![],
        }
    }
}
impl Sentinel for RecordingSentinel {
    type Error = Infallible;
}
impl<K: Debug> Visit<K> for RecordingSentinel {
    fn enter(&mut self, key: &K) -> Result<(), Self::Error> {
        self.path.push(format!("{key:?}"));
        Ok(())
    }
    fn exit(&mut self) -> Result<(), Self::Error> {
        self.path.pop();
        Ok(())
    }
}
impl KeySentinel for RecordingSentinel {
    fn create_key(&mut self) -> Result<(), Self::Error> {
        self.changes_seen
            .push(format!("create_key at {}", self.path.join("/")));
        Ok(())
    }

    fn delete_key(&mut self) -> Result<(), Self::Error> {
        self.changes_seen
            .push(format!("delete_key at {}", self.path.join("/")));
        Ok(())
    }
}
impl<V: Debug> ValueSentinel<V> for RecordingSentinel {
    fn set(&mut self, value: &V) -> Result<(), Self::Error> {
        self.changes_seen.push(format!("set {value:?}"));
        Ok(())
    }
    fn unset(&mut self, value: V) -> Result<(), Self::Error> {
        self.changes_seen.push(format!("unset {:?}", &value));
        Ok(())
    }
}
impl<V: Debug> TypeSentinel<V> for RecordingSentinel {
    fn set_type(&mut self, value_type: ValueType<V>) -> Result<(), Self::Error> {
        self.changes_seen.push(format!("set_type {value_type:?}"));
        Ok(())
    }
    fn unset_type(&mut self, value_type: ValueType<V>) -> Result<(), Self::Error> {
        self.changes_seen.push(format!("unset_type {value_type:?}"));
        Ok(())
    }
}

pub trait CausalDotStoreExt<DS> {
    fn test_join<S>(
        &self,
        other: &Self,
        on_dot_change: &mut dyn FnMut(DotChange),
        sentinel: &mut S,
    ) -> Result<CausalDotStore<DS>, S::Error>
    where
        S: Sentinel,
        DS: DotStoreJoin<S> + DotStoreJoin<RecordingSentinel> + Default + Clone;
    fn test_join_with_and_track<S>(
        &mut self,
        store: DS,
        context: &CausalContext,
        on_dot_change: &mut dyn FnMut(DotChange),
        sentinel: &mut S,
    ) -> Result<(), S::Error>
    where
        DS: DotStoreJoin<S> + DotStoreJoin<RecordingSentinel> + Clone + Default,
        S: Sentinel;
    fn reimpl_join_with_and_track<S>(
        &mut self,
        store: DS,
        context: &CausalContext,
        on_dot_change: &mut dyn FnMut(DotChange),
        sentinel: &mut S,
    ) -> Result<(), S::Error>
    where
        DS: DotStoreJoin<S> + Default,
        S: Sentinel;
}

impl<DS> CausalDotStoreExt<DS> for CausalDotStore<DS>
where
    DS: DotStoreJoin<RecordingSentinel> + Default + Clone,
{
    fn test_join<S>(
        &self,
        other: &Self,
        on_dot_change: &mut dyn FnMut(DotChange),
        sentinel: &mut S,
    ) -> Result<CausalDotStore<DS>, S::Error>
    where
        S: Sentinel,
        DS: DotStoreJoin<S> + DotStoreJoin<RecordingSentinel> + Default + Clone,
    {
        let mut this = self.clone();
        this.test_join_with_and_track(
            other.store.clone(),
            &other.context,
            on_dot_change,
            sentinel,
        )?;

        Ok(CausalDotStore {
            store: this.store,
            context: this.context,
        })
    }

    fn test_join_with_and_track<S>(
        &mut self,
        store: DS,
        context: &CausalContext,
        on_dot_change: &mut dyn FnMut(DotChange),
        sentinel: &mut S,
    ) -> Result<(), S::Error>
    where
        DS: DotStoreJoin<S> + DotStoreJoin<RecordingSentinel> + Clone + Default,
        S: Sentinel,
    {
        #[cfg(debug_assertions)]
        {
            // We do a dry_join here first, to ensure that dry-join
            // and join always result in the same set of calls being
            // made to Sentinel. This is an invariant that we want to always
            // hold, so we check it in debug builds for all test cases using this function.

            let mut dry_join_sentinel = RecordingSentinel::new();
            let dry_result = <DS as DotStoreJoin<RecordingSentinel>>::dry_join(
                (&self.store, &self.context),
                (&store, context),
                &mut dry_join_sentinel,
            )
            .expect("RecordingSentinel is infallible");

            let mut full_run_sentinel = RecordingSentinel::new();
            let full_result = DS::join(
                (self.store.clone(), &self.context),
                (store.clone(), context),
                &mut |_| {},
                &mut full_run_sentinel,
            )
            .expect("RecordingSentinel is infallible");

            assert_eq!(
                dry_join_sentinel.changes_seen,
                full_run_sentinel.changes_seen
            );
            assert_eq!(dry_result.is_bottom(), full_result.is_bottom());
        }

        self.reimpl_join_with_and_track(store, context, on_dot_change, sentinel)?;
        Ok(())
    }

    fn reimpl_join_with_and_track<S>(
        &mut self,
        store: DS,
        context: &CausalContext,
        on_dot_change: &mut dyn FnMut(DotChange),
        sentinel: &mut S,
    ) -> Result<(), S::Error>
    where
        DS: DotStoreJoin<S> + Default,
        S: Sentinel,
    {
        let old_store = std::mem::take(&mut self.store);
        self.store = DS::join(
            (old_store, &self.context),
            (store, context),
            on_dot_change,
            sentinel,
        )?;
        self.context.union(context);
        Ok(())
    }
}

/// A Sentinel that counts changes to values and rejects other changes.
///
/// Setting `permissive` to true disables erroring on key and type changes.
#[derive(Debug)]
pub struct ValueCountingValidator<V> {
    pub added: BTreeMap<V, usize>,
    pub removed: BTreeMap<V, usize>,
    path: Vec<String>,
    permissive: bool,
}

impl<V> Default for ValueCountingValidator<V> {
    fn default() -> Self {
        Self {
            added: Default::default(),
            removed: Default::default(),
            path: Default::default(),
            permissive: false,
        }
    }
}

impl<V> ValueCountingValidator<V> {
    pub fn new(permissive: bool) -> Self {
        Self {
            permissive,
            ..Default::default()
        }
    }
}

impl<V> Sentinel for ValueCountingValidator<V> {
    type Error = String;
}

impl<K, V> Visit<K> for ValueCountingValidator<V>
where
    K: std::fmt::Debug,
{
    fn enter(&mut self, key: &K) -> Result<(), Self::Error> {
        self.path.push(format!("{key:?}"));
        Ok(())
    }

    fn exit(&mut self) -> Result<(), Self::Error> {
        self.path.pop();
        Ok(())
    }
}

impl<V> KeySentinel for ValueCountingValidator<V> {
    fn create_key(&mut self) -> Result<(), Self::Error> {
        self.permissive
            .then_some(())
            .ok_or(format!("create_key at {}", self.path.join("/")))
    }

    fn delete_key(&mut self) -> Result<(), Self::Error> {
        self.permissive
            .then_some(())
            .ok_or(format!("delete_key at {}", self.path.join("/")))
    }
}

impl<C, V> TypeSentinel<C> for ValueCountingValidator<V>
where
    C: Debug,
{
    fn set_type(&mut self, value_type: dson::crdts::ValueType<C>) -> Result<(), Self::Error> {
        self.permissive.then_some(()).ok_or(format!(
            "set_type: {value_type:?} at {}",
            self.path.join("/")
        ))
    }

    fn unset_type(&mut self, value_type: dson::crdts::ValueType<C>) -> Result<(), Self::Error> {
        self.permissive.then_some(()).ok_or(format!(
            "unset_type: {value_type:?} at {}",
            self.path.join("/")
        ))
    }
}

impl<V> ValueSentinel<V> for ValueCountingValidator<V>
where
    V: std::fmt::Debug + Ord + Clone,
{
    fn set(&mut self, value: &V) -> Result<(), Self::Error> {
        *self.added.entry(value.clone()).or_default() += 1;
        Ok(())
    }

    fn unset(&mut self, value: V) -> Result<(), Self::Error> {
        *self.removed.entry(value).or_default() += 1;
        Ok(())
    }
}
