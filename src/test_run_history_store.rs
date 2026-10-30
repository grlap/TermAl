// Maintained candidate indexes for the retained test-run history maps.
// Owns the private store wrappers for test_run_observations, test_run_compacted
// and test_run_registrations: immutable Deref to the underlying BTreeMap, every
// mutation through a helper that keeps the derived indexes equal to the rows,
// and the persistence stamp of the wrapped PersistTracked map. Index membership
// is only a candidate filter: callers re-apply their original predicates, and
// buckets yield candidates in the maps' own key order. Does not own matching
// predicates, persistence, publication or compaction policy. New file, not split
// from another.
mod test_run_history_store {
    use super::{
        EngramAuthorityStoreKey, PersistMetadataStamp, PersistTracked, TestRunCommandObservation,
        TestRunCompacted, TestRunEvidenceAssociation, TestRunRegistration,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::ops::Deref;

    // Test-only count of index maintenance operations (one per row whose
    // memberships are removed or added), for zero-maintenance witnesses.
    #[cfg(test)]
    thread_local! {
        pub(super) static TEST_HISTORY_INDEX_MAINTENANCE: std::cell::Cell<usize> =
            const { std::cell::Cell::new(0) };
        // Off by default: every test enforces the update_unindexed_fields
        // contract unless a timing witness holds SkipContractGuardForTiming.
        static TEST_SKIP_CONTRACT_GUARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Test-only scope for timing witnesses: skips the update_unindexed_fields
    /// contract guard so a timing measures the production path, and restores
    /// the previous setting on drop, including during a panic.
    #[cfg(test)]
    pub(super) struct SkipContractGuardForTiming {
        previous: bool,
    }

    #[cfg(test)]
    impl SkipContractGuardForTiming {
        pub(super) fn new() -> Self {
            Self { previous: TEST_SKIP_CONTRACT_GUARD.with(|skip| skip.replace(true)) }
        }
    }

    #[cfg(test)]
    impl Drop for SkipContractGuardForTiming {
        fn drop(&mut self) {
            TEST_SKIP_CONTRACT_GUARD.with(|skip| skip.set(self.previous));
        }
    }

    #[cfg(test)]
    pub(super) fn contract_guard_skipped() -> bool {
        TEST_SKIP_CONTRACT_GUARD.with(std::cell::Cell::get)
    }

    fn count_maintenance() {
        #[cfg(test)]
        TEST_HISTORY_INDEX_MAINTENANCE.with(|count| count.set(count.get() + 1));
    }

    /// Keeps an index equal to the rows it was built from.
    pub(super) trait RowIndex<V>: Default {
        fn add(&mut self, key: &str, row: &V);
        fn remove(&mut self, key: &str, row: &V);
    }

    /// A key's membership in one bucket of a derived index.
    fn add_member<K: Ord>(map: &mut BTreeMap<K, BTreeSet<String>>, bucket: K, key: &str) {
        map.entry(bucket).or_default().insert(key.to_owned());
    }

    fn remove_member<K: Ord>(map: &mut BTreeMap<K, BTreeSet<String>>, bucket: &K, key: &str) {
        if let Some(members) = map.get_mut(bucket) {
            members.remove(key);
            if members.is_empty() {
                map.remove(bucket);
            }
        }
    }

    /// Candidates of one bucket, in map key order.
    fn members<'a, K: Ord + ?Sized, Q: Ord>(map: &'a BTreeMap<Q, BTreeSet<String>>, bucket: &K)
        -> impl Iterator<Item = &'a String> + 'a
    where
        Q: std::borrow::Borrow<K>,
    {
        map.get(bucket).into_iter().flatten()
    }

    /// A history map whose derived index always equals its rows. The rows are
    /// private: there is no DerefMut and no mutable iterator, so every mutation
    /// goes through a helper that maintains the index.
    pub(super) struct IndexedRows<V, I> {
        rows: PersistTracked<BTreeMap<String, V>>,
        index: I,
    }

    impl<V, I: RowIndex<V>> IndexedRows<V, I> {
        fn build(rows: PersistTracked<BTreeMap<String, V>>) -> Self {
            let mut index = I::default();
            for (key, row) in rows.iter() {
                index.add(key, row);
            }
            Self { rows, index }
        }

        pub(super) fn index(&self) -> &I {
            &self.index
        }

        pub(super) fn stamp(&self) -> PersistMetadataStamp {
            self.rows.stamp()
        }

        /// Takes a mutable borrow of the rows, which stamps them for capture
        /// exactly as a direct mutable borrow of the PersistTracked map did.
        fn rows_mut(&mut self) -> &mut BTreeMap<String, V> {
            &mut self.rows
        }

        pub(super) fn insert(&mut self, key: String, row: V) -> Option<V> {
            count_maintenance();
            if let Some(previous) = self.rows.get(&key) {
                self.index.remove(&key, previous);
            }
            self.index.add(&key, &row);
            self.rows_mut().insert(key, row)
        }

        pub(super) fn remove(&mut self, key: &str) -> Option<V> {
            let removed = self.rows_mut().remove(key)?;
            count_maintenance();
            self.index.remove(key, &removed);
            Some(removed)
        }

        /// Mutates one row and reindexes it under its actual map key. The
        /// closure cannot keep the borrow: its result may not reference the row.
        pub(super) fn update<R>(&mut self, key: &str, change: impl FnOnce(&mut V) -> R) -> Option<R> {
            // Borrow the two fields separately; the rows' borrow stamps them.
            let rows: &mut BTreeMap<String, V> = &mut self.rows;
            let row = rows.get_mut(key)?;
            count_maintenance();
            self.index.remove(key, row);
            let result = change(row);
            self.index.add(key, row);
            Some(result)
        }

        /// Mutates one row WITHOUT index maintenance. Contract: the closure
        /// must not change any indexed field. Those are, for observations:
        /// each evidence association's store, binding root execution, work,
        /// run and claim ids and producer observation id (and the evidence
        /// list's membership); summary owner session and run id; grant and
        /// tool identities; artifact presence and artifact run id. For
        /// compacted proofs: summary run id, and each nested observation's
        /// execution id and association store and binding ids. For
        /// registrations: run id. Only result-filling paths use it (lifecycle
        /// read, verification record, checkpoint, confirmed cursor). Test
        /// builds rebuild the row's memberships before and after and panic
        /// on any difference.
        pub(super) fn update_unindexed_fields<R>(
            &mut self,
            key: &str,
            change: impl FnOnce(&mut V) -> R,
        ) -> Option<R>
        where
            I: PartialEq + std::fmt::Debug,
        {
            let rows: &mut BTreeMap<String, V> = &mut self.rows;
            let row = rows.get_mut(key)?;
            #[cfg(test)]
            let before = (!contract_guard_skipped()).then(|| {
                let mut memberships = I::default();
                memberships.add(key, row);
                memberships
            });
            let result = change(row);
            #[cfg(test)]
            {
                if let Some(before) = before {
                    let mut after = I::default();
                    after.add(key, row);
                    assert_eq!(before, after, "update_unindexed_fields changed an indexed field of {key}");
                }
            }
            Some(result)
        }

        pub(super) fn retain(&mut self, mut keep: impl FnMut(&str, &V) -> bool) {
            // Stamp even when nothing is dropped, as the map's own retain did.
            self.rows_mut();
            let dropped: Vec<String> = self
                .rows
                .iter()
                .filter(|(key, row)| !keep(key, row))
                .map(|(key, _)| key.clone())
                .collect();
            for key in dropped {
                self.remove(&key);
            }
        }

        pub(super) fn clear(&mut self) {
            self.rows_mut().clear();
            self.index = I::default();
        }

        /// Rebuilds the index from scratch, for test-only consistency oracles.
        #[cfg(test)]
        pub(super) fn rebuilt_index(&self) -> I {
            let mut index = I::default();
            for (key, row) in self.rows.iter() {
                index.add(key, row);
            }
            index
        }

        /// Test-only mutable access to one row. The guard takes the row out of
        /// the index and puts its current fields back when dropped, so the
        /// index equals the rows again once the guard is gone.
        #[cfg(test)]
        pub(super) fn get_mut(&mut self, key: &str) -> Option<RowGuard<'_, V, I>> {
            let row = self.rows.get(key)?;
            count_maintenance();
            self.index.remove(key, row);
            Some(RowGuard { store: self, key: key.to_owned() })
        }

        /// Test-only mutation of every row, each reindexed.
        #[cfg(test)]
        pub(super) fn for_each_mut(&mut self, mut change: impl FnMut(&mut V)) {
            let keys: Vec<String> = self.rows.keys().cloned().collect();
            for key in keys {
                self.update(&key, &mut change);
            }
        }
    }

    #[cfg(test)]
    pub(super) struct RowGuard<'a, V, I: RowIndex<V>> {
        store: &'a mut IndexedRows<V, I>,
        key: String,
    }

    #[cfg(test)]
    impl<V, I: RowIndex<V>> Deref for RowGuard<'_, V, I> {
        type Target = V;
        fn deref(&self) -> &V {
            self.store.rows.get(&self.key).expect("guarded row")
        }
    }

    #[cfg(test)]
    impl<V, I: RowIndex<V>> std::ops::DerefMut for RowGuard<'_, V, I> {
        fn deref_mut(&mut self) -> &mut V {
            let rows: &mut BTreeMap<String, V> = &mut self.store.rows;
            rows.get_mut(&self.key).expect("guarded row")
        }
    }

    #[cfg(test)]
    impl<V, I: RowIndex<V>> Drop for RowGuard<'_, V, I> {
        fn drop(&mut self) {
            let store = &mut *self.store;
            if let Some(row) = store.rows.get(&self.key) {
                store.index.add(&self.key, row);
            }
        }
    }

    impl<V: Clone, I: Clone> Clone for IndexedRows<V, I> {
        fn clone(&self) -> Self {
            Self { rows: self.rows.snapshot_clone(), index: self.index.clone() }
        }
    }

    impl<V: PartialEq, I> PartialEq for IndexedRows<V, I> {
        fn eq(&self, other: &Self) -> bool {
            *self.rows == *other.rows
        }
    }

    impl<V: PartialEq, I> PartialEq<BTreeMap<String, V>> for IndexedRows<V, I> {
        fn eq(&self, other: &BTreeMap<String, V>) -> bool {
            *self.rows == *other
        }
    }

    impl<V, I> Deref for IndexedRows<V, I> {
        type Target = BTreeMap<String, V>;
        fn deref(&self) -> &BTreeMap<String, V> {
            &self.rows
        }
    }

    impl<V, I: RowIndex<V>> Default for IndexedRows<V, I> {
        fn default() -> Self {
            Self::build(PersistTracked::default())
        }
    }

    impl<V, I: RowIndex<V>> From<PersistTracked<BTreeMap<String, V>>> for IndexedRows<V, I> {
        fn from(rows: PersistTracked<BTreeMap<String, V>>) -> Self {
            Self::build(rows)
        }
    }

    impl<V, I: RowIndex<V>> FromIterator<(String, V)> for IndexedRows<V, I> {
        fn from_iter<T: IntoIterator<Item = (String, V)>>(iter: T) -> Self {
            Self::build(iter.into_iter().collect())
        }
    }

    impl<'a, V, I> IntoIterator for &'a IndexedRows<V, I> {
        type Item = (&'a String, &'a V);
        type IntoIter = std::collections::btree_map::Iter<'a, String, V>;
        fn into_iter(self) -> Self::IntoIter {
            self.rows.iter()
        }
    }

    /// Test-only consuming iteration over the rows, as the map's own.
    #[cfg(test)]
    impl<V, I> IntoIterator for IndexedRows<V, I> {
        type Item = (String, V);
        type IntoIter = std::collections::btree_map::IntoIter<String, V>;
        fn into_iter(mut self) -> Self::IntoIter {
            std::mem::take(&mut *self.rows).into_iter()
        }
    }

    impl<V: std::fmt::Debug, I> std::fmt::Debug for IndexedRows<V, I> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.rows.fmt(f)
        }
    }

    /// The exact lifecycle a revalidation read authorizes: store plus the
    /// binding's root execution, work, run and claim.
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub(super) struct TestRunLifecycleKey {
        store: EngramAuthorityStoreKey,
        root_execution_id: String,
        work_id: String,
        run_id: String,
        claim_id: String,
    }

    impl TestRunLifecycleKey {
        pub(super) fn new(
            store: &EngramAuthorityStoreKey,
            root_execution_id: &str,
            work_id: &str,
            run_id: &str,
            claim_id: &str,
        ) -> Self {
            Self {
                store: store.clone(),
                root_execution_id: root_execution_id.to_owned(),
                work_id: work_id.to_owned(),
                run_id: run_id.to_owned(),
                claim_id: claim_id.to_owned(),
            }
        }

        fn of(association: &TestRunEvidenceAssociation) -> Self {
            Self::new(
                &association.store,
                &association.binding.root_execution_id,
                &association.binding.work_id,
                &association.binding.run_id,
                &association.binding.claim_id,
            )
        }
    }

    /// The bucket a reported check's association scan narrows to: owner,
    /// grant and tool. Fingerprint, start time and worktree stay predicates.
    pub(super) type TestRunCheckBucket = (Option<String>, Option<String>, String);

    fn check_bucket(row: &TestRunCommandObservation) -> TestRunCheckBucket {
        (row.summary.owner_session_id.clone(), row.grant_identity.clone(), row.tool_identity.clone())
    }

    /// Candidate indexes over live observations, keyed by actual map key.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub(super) struct TestRunObservationIndex {
        lifecycle: BTreeMap<TestRunLifecycleKey, BTreeSet<String>>,
        producer: BTreeMap<String, BTreeSet<String>>,
        check: BTreeMap<TestRunCheckBucket, BTreeSet<String>>,
        summary_run: BTreeMap<String, BTreeSet<String>>,
        artifact_run: BTreeMap<String, BTreeSet<String>>,
    }

    impl RowIndex<TestRunCommandObservation> for TestRunObservationIndex {
        fn add(&mut self, key: &str, row: &TestRunCommandObservation) {
            for association in &row.evidence {
                add_member(&mut self.lifecycle, TestRunLifecycleKey::of(association), key);
                add_member(&mut self.producer, association.producer_observation_id.clone(), key);
            }
            add_member(&mut self.check, check_bucket(row), key);
            add_member(&mut self.summary_run, row.summary.run_id.clone(), key);
            if let Some(artifact) = &row.artifact {
                add_member(&mut self.artifact_run, artifact.run_id.clone(), key);
            }
        }

        fn remove(&mut self, key: &str, row: &TestRunCommandObservation) {
            for association in &row.evidence {
                remove_member(&mut self.lifecycle, &TestRunLifecycleKey::of(association), key);
                remove_member(&mut self.producer, &association.producer_observation_id, key);
            }
            remove_member(&mut self.check, &check_bucket(row), key);
            remove_member(&mut self.summary_run, &row.summary.run_id, key);
            if let Some(artifact) = &row.artifact {
                remove_member(&mut self.artifact_run, &artifact.run_id, key);
            }
        }
    }

    impl TestRunObservationIndex {
        pub(super) fn lifecycle(&self, key: &TestRunLifecycleKey) -> impl Iterator<Item = &String> {
            members(&self.lifecycle, key)
        }
        pub(super) fn producer(&self, observation_id: &str) -> impl Iterator<Item = &String> {
            members(&self.producer, observation_id)
        }
        pub(super) fn check(&self, bucket: &TestRunCheckBucket) -> impl Iterator<Item = &String> {
            members(&self.check, bucket)
        }
        pub(super) fn summary_run(&self, run_id: &str) -> impl Iterator<Item = &String> {
            members(&self.summary_run, run_id)
        }
        pub(super) fn artifact_run(&self, run_id: &str) -> impl Iterator<Item = &String> {
            members(&self.artifact_run, run_id)
        }
    }

    /// Candidate indexes over compacted proofs, keyed by actual map key.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub(super) struct TestRunCompactedIndex {
        lifecycle: BTreeMap<TestRunLifecycleKey, BTreeSet<String>>,
        summary_run: BTreeMap<String, BTreeSet<String>>,
        execution: BTreeMap<String, BTreeSet<String>>,
    }

    impl RowIndex<TestRunCompacted> for TestRunCompactedIndex {
        fn add(&mut self, key: &str, row: &TestRunCompacted) {
            for observation in &row.observations {
                for association in &observation.evidence {
                    add_member(&mut self.lifecycle, TestRunLifecycleKey::of(association), key);
                }
                add_member(&mut self.execution, observation.execution_id.clone(), key);
            }
            add_member(&mut self.summary_run, row.summary.run_id.clone(), key);
        }

        fn remove(&mut self, key: &str, row: &TestRunCompacted) {
            for observation in &row.observations {
                for association in &observation.evidence {
                    remove_member(&mut self.lifecycle, &TestRunLifecycleKey::of(association), key);
                }
                remove_member(&mut self.execution, &observation.execution_id, key);
            }
            remove_member(&mut self.summary_run, &row.summary.run_id, key);
        }
    }

    impl TestRunCompactedIndex {
        pub(super) fn lifecycle(&self, key: &TestRunLifecycleKey) -> impl Iterator<Item = &String> {
            members(&self.lifecycle, key)
        }
        pub(super) fn summary_run(&self, run_id: &str) -> impl Iterator<Item = &String> {
            members(&self.summary_run, run_id)
        }
        pub(super) fn execution(&self, execution_id: &str) -> impl Iterator<Item = &String> {
            members(&self.execution, execution_id)
        }
    }

    /// Registration map keys by the run id each registration carries.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub(super) struct TestRunRegistrationIndex {
        run: BTreeMap<String, BTreeSet<String>>,
    }

    impl RowIndex<TestRunRegistration> for TestRunRegistrationIndex {
        fn add(&mut self, key: &str, row: &TestRunRegistration) {
            add_member(&mut self.run, row.run_id.clone(), key);
        }
        fn remove(&mut self, key: &str, row: &TestRunRegistration) {
            remove_member(&mut self.run, &row.run_id, key);
        }
    }

    impl TestRunRegistrationIndex {
        pub(super) fn run(&self, run_id: &str) -> impl Iterator<Item = &String> {
            members(&self.run, run_id)
        }
    }

    pub(super) type TestRunObservationStore = IndexedRows<TestRunCommandObservation, TestRunObservationIndex>;
    pub(super) type TestRunCompactedStore = IndexedRows<TestRunCompacted, TestRunCompactedIndex>;
    pub(super) type TestRunRegistrationStore = IndexedRows<TestRunRegistration, TestRunRegistrationIndex>;
}

use test_run_history_store::{
    TestRunCompactedStore, TestRunLifecycleKey, TestRunObservationStore, TestRunRegistrationStore,
};
#[cfg(test)]
use test_run_history_store::{
    contract_guard_skipped, SkipContractGuardForTiming, TEST_HISTORY_INDEX_MAINTENANCE,
};
