use crate::keep_alive::AttemptFut;
use crate::utils::client::ClientConnection;
use selium_protocol::BiStream;

pub trait KeepAliveStream {
    type Headers: Sized + Clone + Unpin + Send + 'static;

    fn reestablish_connection(connection: ClientConnection, headers: Self::Headers) -> AttemptFut;
    fn on_reconnect(&mut self, stream: BiStream);
    fn get_connection(&self) -> ClientConnection;
    fn get_headers(&self) -> Self::Headers;
}
