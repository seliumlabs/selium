use std::{
    ops::Deref,
    sync::{Arc, Mutex},
};

/// Generates reusable, unique identifiers
pub struct IdFactory(Mutex<Vec<u16>>);

/// An Id that is automatically freed on drop
#[derive(Clone)]
pub struct Id(Option<u16>, Arc<IdFactory>);

impl IdFactory {
    pub fn new() -> Arc<IdFactory> {
        let ids = Vec::from_iter(1..=u16::MAX);
        Arc::new(Self(Mutex::new(ids)))
    }

    // @todo Handle error conditions like an adult
    pub fn generate(self: &Arc<Self>) -> Id {
        let mut ids = self.0.lock().expect("IdFactory lock poisoned");
        let (idx, id) = ids
            .iter()
            .enumerate()
            .find_map(|(idx, id)| if *id > 0 { Some((idx, *id)) } else { None })
            .expect("ran out of Ids");
        ids[idx] = 0;
        Id::new(id, self.clone())
    }

    fn free(&self, id: u16) {
        let mut ids = self.0.lock().expect("IdFactory lock poisoned");

        // Prevent double-free
        if ids.contains(&id) {
            return;
        }

        let (idx, _) = ids.iter().enumerate().find(|(_, id)| **id == 0).unwrap();
        ids[idx] = id;
    }
}

impl Id {
    /// Create a new Id
    pub fn new(id: u16, factory: Arc<IdFactory>) -> Self {
        Self(Some(id), factory)
    }

    /// Retrieve the Id
    pub fn get(&self) -> u16 {
        self.0.unwrap()
    }
}

impl Drop for Id {
    fn drop(&mut self) {
        if let Some(id) = self.0 {
            self.1.free(id);
        }
    }
}

impl Deref for Id {
    type Target = u16;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_freed_ids() {
        let factory = IdFactory::new();
        let id = factory.generate();
        let first = id.get();
        drop(id);

        let reused = factory.generate();
        assert_eq!(reused.get(), first);
    }

    #[test]
    fn double_free_does_not_duplicate_ids() {
        let factory = IdFactory::new();
        let id = factory.generate();
        let first = id.get();
        let clone = id.clone();
        drop(id);
        drop(clone);

        let reused = factory.generate();
        let next = factory.generate();

        assert_eq!(reused.get(), first);
        assert_ne!(next.get(), first);
    }
}
