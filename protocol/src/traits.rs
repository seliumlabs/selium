pub trait ShutdownStream {
    fn shutdown_stream(&mut self);
}

pub trait ShutdownSink {
    fn shutdown_sink(&mut self);
}
