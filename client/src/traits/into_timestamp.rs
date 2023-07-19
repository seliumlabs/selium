pub trait IntoTimestamp {
    fn into_timestamp(self) -> u64;
}

impl IntoTimestamp for u64 {
    fn into_timestamp(self) -> u64 {
        self
    }
}

impl IntoTimestamp for std::time::Duration {
    fn into_timestamp(self) -> u64 {
        self.as_secs()
    }
}

impl IntoTimestamp for chrono::Duration {
    fn into_timestamp(self) -> u64 {
        self.num_seconds()
            .try_into()
            .expect("Timestamp must be a non-negative integer")
    }
}
