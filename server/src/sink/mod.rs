use futures::{sink::With, Future, Sink, SinkExt as _};

mod fanout_channel;
pub use fanout_channel::*;

mod fanout_many;
pub use fanout_many::*;

mod filter;
pub use filter::Filter;

pub trait SinkExt<Item>: Sink<Item> {
    // This is a wrapper around `with` for conceptual symmetry with `StreamExt::map`
    fn map<U, Fut, F, E>(self, f: F) -> With<Self, Item, U, Fut, F>
    where
        F: FnMut(U) -> Fut,
        Fut: Future<Output = Result<Item, E>>,
        E: From<Self::Error>,
        Self: Sized,
    {
        self.with(f)
    }

    fn filter<Fut, F>(self, f: F) -> Filter<Self, Fut, F, Item>
    where
        F: FnMut(&Item) -> Fut,
        Fut: Future<Output = bool>,
        Self: Sized,
    {
        Filter::new(self, f)
    }
}
