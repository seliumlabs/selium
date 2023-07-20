use super::IntoTimestamp;

pub trait ClientConfig {
    fn keep_alive<T: IntoTimestamp>(self, interval: T) -> Self;
}
