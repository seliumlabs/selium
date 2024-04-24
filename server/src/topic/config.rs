use std::{sync::Arc, time::Duration};

pub type SharedTopicConfig = Arc<TopicConfig>;

#[derive(Debug)]
pub struct TopicConfig {
    pub polling_interval: Duration,
}

impl TopicConfig {
    pub fn new(polling_interval: Duration) -> Self {
        Self { polling_interval }
    }
}
