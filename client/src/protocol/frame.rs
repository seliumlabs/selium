use crate::Operation;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum Frame {
    RegisterPublisher(PublisherPayload),
    RegisterSubscriber(SubscriberPayload),
    Message(String),
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct PublisherPayload {
    pub topic: String,
    pub retention_policy: u64,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct SubscriberPayload {
    pub topic: String,
    pub operations: Vec<Operation>,
}
