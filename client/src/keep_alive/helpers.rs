use selium_protocol::error_codes::REPLIER_ALREADY_BOUND;
use selium_std::errors::{QuicError, Result, SeliumError};
use std::{io, task::Poll};

pub fn is_disconnect_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::ConnectionReset | io::ErrorKind::NotConnected
    )
}

pub fn is_bind_error(code: u32) -> bool {
    matches!(code, REPLIER_ALREADY_BOUND)
}

pub fn is_recoverable_error(err: &SeliumError) -> bool {
    let disconnect_error = matches!(err, SeliumError::IoError(err) if is_disconnect_error(&err));
    let timeout_error = matches!(err, SeliumError::Quic(QuicError::ConnectionError(_)));
    let bind_error = matches!(err, SeliumError::OpenStream(code, _) if is_bind_error(*code));

    timeout_error || disconnect_error || bind_error
}

pub fn is_stream_disconnected<Item>(result: &Result<Item>) -> bool {
    matches!(result, Err(err) if is_recoverable_error(err))
}

pub fn is_sink_disconnected(result: &Poll<Result<()>>) -> bool {
    matches!(result, Poll::Ready(Err(err)) if is_recoverable_error(err))
}
