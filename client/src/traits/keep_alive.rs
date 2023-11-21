use crate::connection::SharedConnection;
use crate::keep_alive::AttemptFut;
use selium_protocol::BiStream;

pub trait KeepAliveStream {
    type Headers: Sized + Clone + Unpin + Send + 'static;

    fn reestablish_connection(connection: SharedConnection, headers: Self::Headers) -> AttemptFut;
    fn on_reconnect(&mut self, stream: BiStream);
    fn get_connection(&self) -> SharedConnection;
    fn get_headers(&self) -> Self::Headers;
}
