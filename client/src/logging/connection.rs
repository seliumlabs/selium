pub fn get_cloud_endpoint() {
    tracing::info!("Retrieving Selium server endpoint from Selium Cloud.");
}

pub fn connect_to_address(endpoint: &str) {
    tracing::info!(endpoint, "Connecting to remote address.");
}

pub fn successful_connection(endpoint: &str) {
    tracing::info!(endpoint, "Successfully connected to remote address.");
}
