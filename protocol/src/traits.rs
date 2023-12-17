use std::pin::Pin;

use futures::{stream::BoxStream, Sink};

pub trait ShutdownStream {
    fn shutdown_stream(&mut self);
}

pub trait ShutdownSink {
    fn shutdown_sink(&mut self);
}

impl<'a, T> ShutdownStream for BoxStream<'a, T> {
    fn shutdown_stream(&mut self) {}
}

impl<T, E> ShutdownSink for Pin<Box<dyn Sink<T, Error = E> + Send>> {
    fn shutdown_sink(&mut self) {}
}
