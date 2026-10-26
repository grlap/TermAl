// Runtime-only mutation stamps for the existing metadata-unit capture owner.
// The payload is private to this module: obtaining any mutable borrow stamps
// it, including take/swap through DerefMut. Whole-wrapper moves are detected
// by the field's separately committed identity, never by generation alone.
mod persist_metadata_tracking {
    use std::collections::BTreeMap;
    use std::ops::{Deref, DerefMut};
    use std::sync::Arc;

    #[derive(Clone)]
    pub(super) struct PersistMetadataStamp {
        identity: Arc<()>,
        generation: u64,
    }

    impl PartialEq for PersistMetadataStamp {
        fn eq(&self, other: &Self) -> bool {
            Arc::ptr_eq(&self.identity, &other.identity) && self.generation == other.generation
        }
    }
    impl Eq for PersistMetadataStamp {}

    pub(super) type PersistMetadataWatermarks = BTreeMap<&'static str, PersistMetadataStamp>;

    pub(super) struct PersistTracked<T> {
        value: T,
        stamp: PersistMetadataStamp,
    }

    impl<T> PersistTracked<T> {
        pub(super) fn stamp(&self) -> PersistMetadataStamp {
            self.stamp.clone()
        }

        pub(super) fn snapshot_clone(&self) -> Self
        where
            T: Clone,
        {
            Self {
                value: self.value.clone(),
                stamp: self.stamp.clone(),
            }
        }
    }

    impl<T> From<T> for PersistTracked<T> {
        fn from(value: T) -> Self {
            Self {
                value,
                stamp: PersistMetadataStamp {
                    identity: Arc::new(()),
                    generation: 0,
                },
            }
        }
    }

    impl<T: Default> Default for PersistTracked<T> {
        fn default() -> Self {
            T::default().into()
        }
    }

    impl<T> Deref for PersistTracked<T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.value
        }
    }

    impl<T> DerefMut for PersistTracked<T> {
        fn deref_mut(&mut self) -> &mut T {
            self.stamp.generation = self
                .stamp
                .generation
                .checked_add(1)
                .expect("metadata collection mutation generation exhausted");
            &mut self.value
        }
    }

    impl<A, T: FromIterator<A>> FromIterator<A> for PersistTracked<T> {
        fn from_iter<I: IntoIterator<Item = A>>(iter: I) -> Self {
            T::from_iter(iter).into()
        }
    }

    impl<T: IntoIterator> IntoIterator for PersistTracked<T> {
        type Item = T::Item;
        type IntoIter = T::IntoIter;
        fn into_iter(self) -> Self::IntoIter {
            self.value.into_iter()
        }
    }

    impl<'a, T> IntoIterator for &'a PersistTracked<T>
    where
        &'a T: IntoIterator,
    {
        type Item = <&'a T as IntoIterator>::Item;
        type IntoIter = <&'a T as IntoIterator>::IntoIter;
        fn into_iter(self) -> Self::IntoIter {
            (&self.value).into_iter()
        }
    }

    impl<'a, T> IntoIterator for &'a mut PersistTracked<T>
    where
        &'a mut T: IntoIterator,
    {
        type Item = <&'a mut T as IntoIterator>::Item;
        type IntoIter = <&'a mut T as IntoIterator>::IntoIter;
        fn into_iter(self) -> Self::IntoIter {
            self.deref_mut().into_iter()
        }
    }

    impl<T: serde::Serialize> serde::Serialize for PersistTracked<T> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.value.serialize(serializer)
        }
    }

    impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for PersistTracked<T> {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            T::deserialize(deserializer).map(Self::from)
        }
    }

    impl<T: std::fmt::Debug> std::fmt::Debug for PersistTracked<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.value.fmt(f)
        }
    }

    impl<T: PartialEq> PartialEq for PersistTracked<T> {
        fn eq(&self, other: &Self) -> bool {
            self.value == other.value
        }
    }
    impl<T: Eq> Eq for PersistTracked<T> {}
    impl<T: PartialEq> PartialEq<T> for PersistTracked<T> {
        fn eq(&self, other: &T) -> bool {
            self.value == *other
        }
    }
}

use persist_metadata_tracking::{PersistMetadataStamp, PersistMetadataWatermarks, PersistTracked};
