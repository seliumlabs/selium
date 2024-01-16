use selium_std::errors::SeliumError;

pub fn reconnect_attempt(attempt_num: u32, max_attempts: u32) {
    tracing::info!(
        attempt_num,
        max_attempts,
        "Attempting to reconnect to server..."
    );
}

pub fn too_many_retries() {
    tracing::error!("Too many connection retries. Aborting reconnection.");
}

pub fn successful_reconnection() {
    tracing::info!("Successfully reconnected to server.");
}

pub fn reconnect_error(err: &SeliumError) {
    tracing::error!(error = err.to_string(), "Failed to reconnect to server.");
}

pub fn unrecoverable_error(err: &SeliumError) {
    tracing::error!(
        error = err.to_string(),
        "Encountered unrecoverable error while reconnecting to server."
    );
}

pub fn connection_lost() {
    tracing::error!("Client lost connection to the server.");
}
