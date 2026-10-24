mod message_sequence {
    use super::*;
    // Resident transcripts share immutable individual bodies with persistence
    // snapshots. Every mutable access detaches that body, never the transcript.
    // No mutable Vec or slice escapes this boundary.
    #[derive(Clone, Debug, Default)]
    pub(super) struct MessageSequence {
        bodies: Vec<Arc<Message>>,
    }

    type MessageIter<'a> =
        std::iter::Map<std::slice::Iter<'a, Arc<Message>>, fn(&Arc<Message>) -> &Message>;

    #[cfg(test)]
    thread_local! {
        static SHARED_BODY_CLONES: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    }

    fn into_message(body: Arc<Message>) -> Message {
        #[cfg(test)]
        if Arc::strong_count(&body) > 1 {
            SHARED_BODY_CLONES.with(|counter| {
                if let Some(count) = counter.get() {
                    counter.set(Some(count + 1));
                }
            });
        }
        Arc::unwrap_or_clone(body)
    }

    impl MessageSequence {
        pub(super) fn len(&self) -> usize {
            self.bodies.len()
        }
        pub(super) fn is_empty(&self) -> bool {
            self.bodies.is_empty()
        }
        pub(super) fn iter(&self) -> MessageIter<'_> {
            self.bodies.iter().map(Arc::as_ref)
        }
        pub(super) fn matching_mut(
            &mut self,
            predicate: fn(&Message) -> bool,
        ) -> impl DoubleEndedIterator<Item = (usize, &mut Message)> {
            self.bodies
                .iter_mut()
                .enumerate()
                .filter(move |(_, body)| predicate(body))
                .map(|(index, body)| (index, Arc::make_mut(body)))
        }
        pub(super) fn get(&self, index: usize) -> Option<&Message> {
            self.bodies.get(index).map(Arc::as_ref)
        }
        pub(super) fn get_mut(&mut self, index: usize) -> Option<&mut Message> {
            self.bodies.get_mut(index).map(Arc::make_mut)
        }
        pub(super) fn first(&self) -> Option<&Message> {
            self.get(0)
        }
        pub(super) fn last(&self) -> Option<&Message> {
            self.bodies.last().map(Arc::as_ref)
        }
        pub(super) fn push(&mut self, message: Message) {
            self.bodies.push(Arc::new(message));
        }
        pub(super) fn insert(&mut self, index: usize, message: Message) {
            self.bodies.insert(index, Arc::new(message));
        }
        pub(super) fn remove(&mut self, index: usize) -> Message {
            into_message(self.bodies.remove(index))
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
        #[cfg(test)]
        pub(super) fn retain(&mut self, mut predicate: impl FnMut(&Message) -> bool) {
            self.bodies.retain(|body| predicate(body));
        }
        #[cfg(test)]
        pub(super) fn shares_body(&self, index: usize, other: &Self, other_index: usize) -> bool {
            self.bodies
                .get(index)
                .zip(other.bodies.get(other_index))
                .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
        }
        pub(super) fn identities(&self) -> Vec<std::sync::Weak<Message>> {
            self.bodies.iter().map(Arc::downgrade).collect()
        }
        // Weak pins the old allocation against address reuse without owning
        // its body. make_mut dissociates old weak identities before mutation.
        pub(super) fn matches_identity(
            &self,
            index: usize,
            saved: &std::sync::Weak<Message>,
        ) -> bool {
            self.bodies
                .get(index)
                .is_some_and(|body| std::ptr::eq(saved.as_ptr(), Arc::as_ptr(body)))
        }
        pub(super) fn clear(&mut self) {
            self.bodies.clear();
        }
        pub(super) fn truncate(&mut self, len: usize) {
            self.bodies.truncate(len);
        }
        #[cfg(test)]
        pub(super) fn swap(&mut self, a: usize, b: usize) {
            self.bodies.swap(a, b);
        }
        // Dropping Vec::Drain removes the range without visiting/cloning its
        // shared message bodies. Eviction intentionally produces no messages.
        pub(super) fn discard(&mut self, range: impl std::ops::RangeBounds<usize>) {
            drop(self.bodies.drain(range));
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
            }
        }
    }
    impl From<Vec<Message>> for MessageSequence {
        fn from(messages: Vec<Message>) -> Self {
            messages.into_iter().collect()
        }
    }
    impl FromIterator<Message> for MessageSequence {
        fn from_iter<T: IntoIterator<Item = Message>>(messages: T) -> Self {
            Self {
                bodies: messages.into_iter().map(Arc::new).collect(),
            }
        }
    }
    impl Extend<Message> for MessageSequence {
        fn extend<T: IntoIterator<Item = Message>>(&mut self, messages: T) {
            self.bodies.extend(messages.into_iter().map(Arc::new));
        }
    }
    impl std::ops::Index<usize> for MessageSequence {
        type Output = Message;
        fn index(&self, index: usize) -> &Message {
            &self.bodies[index]
        }
    }
    impl std::ops::IndexMut<usize> for MessageSequence {
        fn index_mut(&mut self, index: usize) -> &mut Message {
            Arc::make_mut(&mut self.bodies[index])
        }
    }
    impl<'a> IntoIterator for &'a MessageSequence {
        type Item = &'a Message;
        type IntoIter = MessageIter<'a>;
        fn into_iter(self) -> Self::IntoIter {
            self.iter()
        }
    }
    impl IntoIterator for MessageSequence {
        type Item = Message;
        type IntoIter =
            std::iter::Map<std::vec::IntoIter<Arc<Message>>, fn(Arc<Message>) -> Message>;
        fn into_iter(self) -> Self::IntoIter {
            self.bodies.into_iter().map(into_message)
        }
    }
    impl Serialize for MessageSequence {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            serializer.collect_seq(self.iter())
        }
    }
    impl<'de> Deserialize<'de> for MessageSequence {
        fn deserialize<D: serde::Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            Vec::<Message>::deserialize(deserializer).map(Into::into)
        }
    }
}
use message_sequence::MessageSequence;
