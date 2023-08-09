use std::{
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, MutexGuard},
};

pub(super) struct Lockable<'a, T> {
    inner: Arc<Mutex<T>>,
    guard: MutexGuard<'a, T>,
}

impl<'a, T> Lockable<'a, T> {
    pub fn new(inner: Arc<Mutex<T>>) -> Self {
        let guard = inner.try_lock().expect("Lockable type must not be locked");

        Self { inner, guard }
    }
}

impl<'a, T> Deref for Lockable<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a, T> DerefMut for Lockable<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
