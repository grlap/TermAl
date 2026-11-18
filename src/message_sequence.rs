mod message_sequence {
    use super::*;
    // Resident transcripts and session inputs share individual row values.
    // Every mutable access detaches that row. Runtime handles retain their
    // existing sharing; this does not freeze their interior state.
    // No mutable Vec or slice escapes this boundary.
    #[derive(Debug)]
    pub(super) struct SharedSequence<E> {
        bodies: Vec<Arc<E>>,
        // Session persistence opts into slot tracking. None means that no
        // keyed view has been built, or a structural change invalidated it.
        touched_slots: Option<BTreeSet<usize>>,
    }

    type SharedIter<'a, E> = std::iter::Map<std::slice::Iter<'a, Arc<E>>, fn(&Arc<E>) -> &E>;

    impl<E> Clone for SharedSequence<E> {
        fn clone(&self) -> Self {
            // Slot tracking belongs to the live sequence and its keyed view.
            Self {
                bodies: self.bodies.clone(),
                touched_slots: None,
            }
        }
    }

    #[cfg(test)]
    thread_local! {
        static SHARED_BODY_CLONES: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    }

    fn into_value<E: Clone + 'static>(body: Arc<E>) -> E {
        // Keep the existing transcript-copy observer specific to messages.
        #[cfg(test)]
        if std::any::TypeId::of::<E>() == std::any::TypeId::of::<Message>()
            && Arc::strong_count(&body) > 1
        {
            SHARED_BODY_CLONES.with(|counter| {
                if let Some(count) = counter.get() {
                    counter.set(Some(count + 1));
                }
            });
        }
        Arc::unwrap_or_clone(body)
    }

    impl<E> Default for SharedSequence<E> {
        fn default() -> Self {
            Self {
                bodies: Vec::new(),
                touched_slots: None,
            }
        }
    }

    impl<E: Clone + 'static> SharedSequence<E> {
        pub(super) fn len(&self) -> usize {
            self.bodies.len()
        }
        pub(super) fn is_empty(&self) -> bool {
            self.bodies.is_empty()
        }
        pub(super) fn iter(&self) -> SharedIter<'_, E> {
            self.bodies.iter().map(Arc::as_ref)
        }
        pub(super) fn matching_mut(
            &mut self,
            mut predicate: impl FnMut(&E) -> bool,
        ) -> impl DoubleEndedIterator<Item = (usize, &mut E)> {
            let touched = &mut self.touched_slots;
            self.bodies
                .iter_mut()
                .enumerate()
                .filter(move |(_, body)| predicate(body))
                .map(move |(index, body)| {
                    note_slot(touched, index);
                    (index, Arc::make_mut(body))
                })
        }
        pub(super) fn iter_mut(
            &mut self,
        ) -> impl DoubleEndedIterator<Item = &mut E> + ExactSizeIterator {
            let touched = &mut self.touched_slots;
            self.bodies.iter_mut().enumerate().map(move |(index, body)| {
                note_slot(touched, index);
                Arc::make_mut(body)
            })
        }
        #[cfg(test)]
        pub(super) fn last_mut(&mut self) -> Option<&mut E> {
            self.get_mut(self.len().checked_sub(1)?)
        }
        pub(super) fn get(&self, index: usize) -> Option<&E> {
            self.bodies.get(index).map(Arc::as_ref)
        }
        pub(super) fn get_mut(&mut self, index: usize) -> Option<&mut E> {
            let body = self.bodies.get_mut(index)?;
            note_slot(&mut self.touched_slots, index);
            Some(Arc::make_mut(body))
        }

        pub(super) fn take_touched_slots(&mut self) -> Option<BTreeSet<usize>> {
            self.touched_slots.replace(BTreeSet::new())
        }

        #[cfg(test)]
        pub(super) fn tracked_slot_count(&self) -> Option<usize> {
            self.touched_slots.as_ref().map(BTreeSet::len)
        }

        pub(super) fn invalidate_slots(&mut self) {
            self.touched_slots = None;
        }
        pub(super) fn first(&self) -> Option<&E> {
            self.get(0)
        }
        pub(super) fn last(&self) -> Option<&E> {
            self.bodies.last().map(Arc::as_ref)
        }
        pub(super) fn push(&mut self, message: E) {
            note_slot(&mut self.touched_slots, self.bodies.len());
            self.bodies.push(Arc::new(message));
        }
        pub(super) fn insert(&mut self, index: usize, message: E) {
            self.bodies.insert(index, Arc::new(message));
            self.invalidate_slots();
        }
        pub(super) fn remove(&mut self, index: usize) -> E {
            let removed = self.bodies.remove(index);
            self.invalidate_slots();
            into_value(removed)
        }
        #[cfg(test)]
        pub(super) fn observe_shared_body_clones<T>(operation: impl FnOnce() -> T) -> (T, usize) {
            struct Reset;
            impl Drop for Reset {
                fn drop(&mut self) {
                    SHARED_BODY_CLONES.with(|counter| counter.set(None));
                }
            }
            assert!(
                SHARED_BODY_CLONES
                    .with(|counter| counter.replace(Some(0)))
                    .is_none()
            );
            let _reset = Reset;
            let result = operation();
            let count = SHARED_BODY_CLONES.with(|counter| counter.get().unwrap());
            (result, count)
        }
        pub(super) fn retain(&mut self, mut predicate: impl FnMut(&E) -> bool) {
            let before = self.len();
            self.bodies.retain(|body| predicate(body));
            if self.len() != before {
                self.invalidate_slots();
            }
        }
        #[cfg(test)]
        pub(super) fn shares_body(&self, index: usize, other: &Self, other_index: usize) -> bool {
            self.bodies
                .get(index)
                .zip(other.bodies.get(other_index))
                .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
        }
        pub(super) fn identities(&self) -> Vec<std::sync::Weak<E>> {
            self.bodies.iter().map(Arc::downgrade).collect()
        }
        // Weak pins the old allocation against address reuse without owning
        // its body. make_mut dissociates old weak identities before mutation.
        pub(super) fn matches_identity(&self, index: usize, saved: &std::sync::Weak<E>) -> bool {
            self.bodies
                .get(index)
                .is_some_and(|body| std::ptr::eq(saved.as_ptr(), Arc::as_ptr(body)))
        }
        pub(super) fn clear(&mut self) {
            self.bodies.clear();
            self.invalidate_slots();
        }
        pub(super) fn truncate(&mut self, len: usize) {
            if len < self.len() {
                self.invalidate_slots();
            }
            self.bodies.truncate(len);
        }
        #[cfg(test)]
        pub(super) fn swap(&mut self, a: usize, b: usize) {
            self.bodies.swap(a, b);
            self.invalidate_slots();
        }
        #[cfg(test)]
        pub(super) fn reverse(&mut self) {
            self.bodies.reverse();
            self.invalidate_slots();
        }
        pub(super) fn window(&self, range: impl std::ops::RangeBounds<usize>) -> Self {
            use std::ops::Bound;
            let start = match range.start_bound() {
                Bound::Included(n) => *n,
                Bound::Excluded(n) => n + 1,
                Bound::Unbounded => 0,
            };
            let end = match range.end_bound() {
                Bound::Included(n) => n + 1,
                Bound::Excluded(n) => *n,
                Bound::Unbounded => self.len(),
            };
            Self {
                bodies: self.bodies[start..end].to_vec(),
                touched_slots: None,
            }
        }
    }
    impl<E: Clone + 'static> From<Vec<E>> for SharedSequence<E> {
        fn from(messages: Vec<E>) -> Self {
            messages.into_iter().collect()
        }
    }
    impl<E: Clone + 'static> FromIterator<E> for SharedSequence<E> {
        fn from_iter<T: IntoIterator<Item = E>>(messages: T) -> Self {
            Self {
                bodies: messages.into_iter().map(Arc::new).collect(),
                touched_slots: None,
            }
        }
    }
    impl<E: Clone + 'static> Extend<E> for SharedSequence<E> {
        fn extend<T: IntoIterator<Item = E>>(&mut self, messages: T) {
            let start = self.len();
            self.bodies.extend(messages.into_iter().map(Arc::new));
            if let Some(touched) = &mut self.touched_slots {
                touched.extend(start..self.bodies.len());
            }
        }
    }
    impl<E> std::ops::Index<usize> for SharedSequence<E> {
        type Output = E;
        fn index(&self, index: usize) -> &E {
            &self.bodies[index]
        }
    }
    impl<E: Clone> std::ops::IndexMut<usize> for SharedSequence<E> {
        fn index_mut(&mut self, index: usize) -> &mut E {
            note_slot(&mut self.touched_slots, index);
            Arc::make_mut(&mut self.bodies[index])
        }
    }
    impl<'a, E: Clone + 'static> IntoIterator for &'a SharedSequence<E> {
        type Item = &'a E;
        type IntoIter = SharedIter<'a, E>;
        fn into_iter(self) -> Self::IntoIter {
            self.iter()
        }
    }
    impl<'a, E: Clone + 'static> IntoIterator for &'a mut SharedSequence<E> {
        type Item = &'a mut E;
        type IntoIter = std::iter::Map<std::slice::IterMut<'a, Arc<E>>, fn(&mut Arc<E>) -> &mut E>;
        fn into_iter(self) -> Self::IntoIter {
            if let Some(touched) = &mut self.touched_slots {
                touched.extend(0..self.bodies.len());
            }
            self.bodies.iter_mut().map(Arc::make_mut)
        }
    }
    impl<E: Clone + 'static> IntoIterator for SharedSequence<E> {
        type Item = E;
        type IntoIter = std::iter::Map<std::vec::IntoIter<Arc<E>>, fn(Arc<E>) -> E>;
        fn into_iter(self) -> Self::IntoIter {
            self.bodies.into_iter().map(into_value)
        }
    }
    impl<E: Clone + Serialize + 'static> Serialize for SharedSequence<E> {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            serializer.collect_seq(self.iter())
        }
    }
    impl<'de, E: Clone + Deserialize<'de> + 'static> Deserialize<'de> for SharedSequence<E> {
        fn deserialize<D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            Vec::<E>::deserialize(deserializer).map(Into::into)
        }
    }

    fn note_slot(touched: &mut Option<BTreeSet<usize>>, index: usize) {
        if let Some(touched) = touched {
            touched.insert(index);
        }
    }
}
use message_sequence::SharedSequence;
type MessageSequence = SharedSequence<Message>;
type SessionSequence = SharedSequence<SessionRecord>;
