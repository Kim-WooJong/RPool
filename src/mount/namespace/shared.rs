//! Copy-on-write sharing for the namespace's large collections.
//!
//! `Shared<T>` dereferences to `T` like the plain field it replaces, so reads
//! (`get`, `iter`, `&state.events` passed as `&BTreeMap`) are unchanged.
//! Cloning only bumps a reference count; the first mutable access of a shared
//! value copies it once ([`Arc::make_mut`]). The drive's
//! `let mut next = s.clone(); ...; next.save(..)?; *s = next;` pattern thus
//! copies only the collections the mutation actually writes.
//!
//! Every mutable access moves or copies the value away from weak observers,
//! so a [`Weak`] taken by [`Shared::downgrade`] identifies exactly one
//! unchanged value: [`Shared::is`] is a sound cache key.
use crate::prelude::*;
use std::sync::Weak;

/// Copy-on-write `Arc` wrapper for one large namespace collection (events,
/// published, receipts, directories, bases); see the module docs.
#[derive(Default)]
pub(crate) struct Shared<T>(Arc<T>);

impl<T> Shared<T> {
    /// Wraps `value` in a new, unshared allocation.
    pub(crate) fn new(value: T) -> Self {
        Self(Arc::new(value))
    }
    /// Whether `a` and `b` are the same, unmodified value.
    pub(crate) fn ptr_eq(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.0, &b.0)
    }
    /// A key for this exact value, invalidated by any later mutable access.
    pub(crate) fn downgrade(&self) -> Weak<T> {
        Arc::downgrade(&self.0)
    }
    /// Whether `key` (from [`Self::downgrade`]) still names this value.
    pub(crate) fn is(&self, key: &Weak<T>) -> bool {
        std::ptr::eq(key.as_ptr(), Arc::as_ptr(&self.0))
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> std::ops::Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Clone> std::ops::DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        // Unique with weak observers: the value moves to a new allocation
        // (no copy), so those observers no longer match. Shared: one copy.
        Arc::make_mut(&mut self.0)
    }
}

impl<T> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: PartialEq> PartialEq for Shared<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl<T: PartialEq> PartialEq<T> for Shared<T> {
    fn eq(&self, other: &T) -> bool {
        *self.0 == *other
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<'a, T> IntoIterator for &'a Shared<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        (&*self.0).into_iter()
    }
}

impl<T: Serialize> Serialize for Shared<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Shared<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_until_written_and_writes_invalidate_keys() {
        let mut a: Shared<BTreeMap<u32, u32>> = Shared::default();
        a.insert(1, 1);
        let key = a.downgrade();
        assert!(a.is(&key));
        let mut b = a.clone();
        assert!(Shared::ptr_eq(&a, &b));
        b.insert(2, 2);
        assert!(!Shared::ptr_eq(&a, &b));
        assert!(a.is(&key) && !b.is(&key));
        assert_eq!(a.len(), 1);
        // A unique value written in place still moves away from its key.
        let _ = &mut *a;
        assert!(!a.is(&key));
        assert_eq!(*a, BTreeMap::from([(1, 1)]));
        assert_eq!(
            serde_json::to_string(&b).unwrap(),
            serde_json::to_string(&*b).unwrap()
        );
    }
}
