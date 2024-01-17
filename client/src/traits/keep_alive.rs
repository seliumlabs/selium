use crate::connection::SharedConnection;
use crate::keep_alive::AttemptFut;
use selium_protocol::BiStream;

/// Provides methods to adapt a stream into a `KeepAlive` compatible stream.
pub trait KeepAliveStream {
    type Headers: Sized + Clone + Unpin + Send + 'static;

    /// Callback that is invoked to attempt to reconnect to the `Selium` server.
    fn reestablish_connection(connection: SharedConnection, headers: Self::Headers, cloud: bool) -> AttemptFut;

    /// Callback that is invoked upon successful reconnection.
    fn on_reconnect(&mut self, stream: BiStream);

    /// Retrieves the shared selium client connection.
    fn get_connection(&self) -> SharedConnection;

    /// Retrieves the headers used to register the stream with the `Selium` server.
    fn get_headers(&self) -> Self::Headers;
}
