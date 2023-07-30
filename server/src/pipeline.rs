use anyhow::Result;
use bytes::Bytes;
use futures::{channel::mpsc::UnboundedSender, future, Future};
use log::{error, info};
use selium::{
    protocol::{PublisherPayload, SubscriberPayload},
    Operation,
};
use std::{
    fmt::{self, Display, Formatter},
    pin::Pin,
    sync::Arc,
};
use tokio::sync::Mutex;

use crate::graph::{hash_key, DoubleEndedTree};

#[derive(Debug)]
enum PipelineNode {
    Publisher(String),
    Subscriber(String, UnboundedSender<(usize, Bytes)>),
    Topic(String),
    Wasm(String),
}

#[derive(Clone, Debug)]
pub struct Pipeline {
    graph: DoubleEndedTree<PipelineNode>,
    write_lock: Arc<Mutex<()>>,
}

impl PipelineNode {
    fn to_string(&self) -> String {
        match self {
            Self::Publisher(h) => format!("Publisher({h})"),
            Self::Subscriber(h, _) => format!("Subscriber({h})"),
            Self::Topic(t) => format!("Topic({t})"),
            Self::Wasm(s) => format!("WASM({s})"),
        }
    }
}

impl Display for PipelineNode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

impl PartialEq for PipelineNode {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}

impl Pipeline {
    pub fn new() -> Self {
        Pipeline {
            graph: DoubleEndedTree::new(),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn add_publisher(&self, hash: &str, payload: PublisherPayload) -> Result<()> {
        info!("Adding publisher {hash} to pipeline");

        let _handle = self.write_lock.lock().await;

        // First add the topic
        let mut left_of = self.graph.add_root(
            payload.topic.clone(),
            PipelineNode::Topic(payload.topic.clone()),
        );

        // Now iterate backwards up the pipe operations towards the socket.
        // We do this so that we can set the next hop.
        for op in payload.operations.into_iter().rev() {
            let key = match op {
                Operation::Filter(f) => f,
                Operation::Map(m) => m,
            };

            left_of = self
                .graph
                .add_left(key.clone(), PipelineNode::Wasm(key), left_of)?;
        }

        // Finally, add the publisher
        self.graph
            .add_left_leaf(hash, PipelineNode::Publisher(hash.to_owned()), left_of)?;

        Ok(())
    }

    pub async fn add_subscriber(
        &self,
        hash: &str,
        payload: SubscriberPayload,
        sock: UnboundedSender<(usize, Bytes)>,
    ) -> Result<()> {
        info!("Adding subscriber {hash} to pipeline");

        let _handle = self.write_lock.lock().await;

        // First add the topic
        let mut right_of = self.graph.add_root(
            payload.topic.clone(),
            PipelineNode::Topic(payload.topic.clone()),
        );

        // Now iterate over the pipe operations towards the socket
        for op in payload.operations.into_iter() {
            let key = match op {
                Operation::Filter(f) => f,
                Operation::Map(m) => m,
            };

            right_of = self
                .graph
                .add_right(key.clone(), PipelineNode::Wasm(key), right_of)?;
        }

        // Finally, add the subscriber
        self.graph.add_right_leaf(
            hash,
            PipelineNode::Subscriber(hash.to_owned(), sock),
            right_of,
        )?;

        Ok(())
    }

    pub async fn rm_publisher(&self, key: &str) -> Result<()> {
        info!("Removing publisher {key} from pipeline");

        let _handle = self.write_lock.lock().await;
        self.graph.rm_left_leaf(key)
    }

    pub async fn rm_subscriber(&self, key: &str) -> Result<()> {
        info!("Removing subscriber {key} from pipeline");

        let _handle = self.write_lock.lock().await;
        self.graph.rm_right_leaf(key)
    }

    pub fn traverse(
        &self,
        publisher: &str,
        message: Bytes,
        sequence: usize,
    ) -> Pin<Box<dyn Future<Output = (usize, Bytes)> + Send>> {
        let key = hash_key(publisher, "left", None);
        self.graph
            .fold_branches((sequence, message), key, |(seq, bytes), node| {
                match node.as_ref() {
                    PipelineNode::Topic(_) | PipelineNode::Publisher(_) => (),
                    PipelineNode::Subscriber(_, sock) => {
                        if let Err(e) = sock.unbounded_send((seq, bytes.clone())) {
                            error!("Failed to send message to subscriber channel: {e}");
                        }
                    }
                    // @TODO - Implement WASM executor
                    PipelineNode::Wasm(_) => (),
                };

                future::ready((seq, bytes))
            })
    }
}

#[cfg(test)]
mod tests {
    use futures::channel::mpsc;

    use super::*;
    use crate::graph::{hash_key, NextHop, Node};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_add_publisher() {
        let pipe: Pipeline = Pipeline::new();

        let hash1 = "127.0.0.1:40009:1";
        let payload1 = PublisherPayload {
            topic: "/namespace/topic".into(),
            retention_policy: 0,
            operations: vec![
                Operation::Map("/namespace/map1".into()),
                Operation::Filter("/namespace/filter1".into()),
                Operation::Map("/namespace/map2".into()),
            ],
        };

        pipe.add_publisher(hash1, payload1).await.unwrap();

        let hash2 = "127.0.0.1:40010:1";
        let payload2 = PublisherPayload {
            topic: "/namespace/topic".into(),
            retention_policy: 0,
            operations: vec![
                Operation::Map("/namespace/map1".into()),
                Operation::Filter("/namespace/filter2".into()),
                Operation::Map("/namespace/map2".into()),
            ],
        };
        pipe.add_publisher(hash2, payload2).await.unwrap();

        let hash3 = "127.0.0.1:40011:1";
        let payload3 = PublisherPayload {
            topic: "/namespace/topic".into(),
            retention_policy: 0,
            operations: vec![
                Operation::Map("/namespace/map1".into()),
                Operation::Filter("/namespace/filter2".into()),
                Operation::Map("/namespace/map2".into()),
            ],
        };
        pipe.add_publisher(hash3, payload3).await.unwrap();

        let topic_key = hash_key("/namespace/topic", "", None);
        let pub1_key = hash_key("127.0.0.1:40009:1", "left", None);
        let pub2_key = hash_key("127.0.0.1:40010:1", "left", None);
        let pub3_key = hash_key("127.0.0.1:40011:1", "left", None);
        let map2_key = hash_key("/namespace/map2", "left", Some(topic_key));
        let filter1_key = hash_key("/namespace/filter1", "left", Some(map2_key));
        let filter2_key = hash_key("/namespace/filter2", "left", Some(map2_key));
        let map11_key = hash_key("/namespace/map1", "left", Some(filter1_key));
        let map12_key = hash_key("/namespace/map1", "left", Some(filter2_key));

        assert_eq!(
            *pipe.graph.get(pub1_key).unwrap(),
            Node::LeftLeaf(
                Arc::new(PipelineNode::Publisher("127.0.0.1:40009:1".to_owned())),
                map11_key
            )
        );

        assert_eq!(
            *pipe.graph.get(pub2_key).unwrap(),
            Node::LeftLeaf(
                Arc::new(PipelineNode::Publisher("127.0.0.1:40010:1".to_owned())),
                map12_key
            )
        );

        assert_eq!(
            *pipe.graph.get(pub3_key).unwrap(),
            Node::LeftLeaf(
                Arc::new(PipelineNode::Publisher("127.0.0.1:40011:1".to_owned())),
                map12_key
            )
        );

        assert_eq!(
            *pipe.graph.get(map11_key).unwrap(),
            Node::Left(
                Arc::new(PipelineNode::Wasm("/namespace/map1".into())),
                filter1_key,
                NextHop::Hop(pub1_key),
            )
        );

        assert_eq!(
            *pipe.graph.get(map12_key).unwrap(),
            Node::Left(
                Arc::new(PipelineNode::Wasm("/namespace/map1".into())),
                filter2_key,
                NextHop::MultiHop(vec![pub2_key, pub3_key]),
            )
        );

        assert_eq!(
            *pipe.graph.get(filter1_key).unwrap(),
            Node::Left(
                Arc::new(PipelineNode::Wasm("/namespace/filter1".into())),
                map2_key,
                NextHop::Hop(map11_key),
            )
        );

        assert_eq!(
            *pipe.graph.get(filter2_key).unwrap(),
            Node::Left(
                Arc::new(PipelineNode::Wasm("/namespace/filter2".into())),
                map2_key,
                NextHop::Hop(map12_key),
            )
        );

        assert_eq!(
            *pipe.graph.get(map2_key).unwrap(),
            Node::Left(
                Arc::new(PipelineNode::Wasm("/namespace/map2".into())),
                topic_key,
                NextHop::MultiHop(vec![filter1_key, filter2_key]),
            )
        );

        assert_eq!(
            *pipe.graph.get(topic_key).unwrap(),
            Node::Root(
                Arc::new(PipelineNode::Topic("/namespace/topic".into())),
                NextHop::None,
                NextHop::Hop(map2_key)
            )
        );
    }

    #[tokio::test]
    async fn test_add_subscriber() {
        let pipe: Pipeline = Pipeline::new();

        let hash1 = "127.0.0.1:40009:1";
        let (tx1, _) = mpsc::unbounded();
        let payload1 = SubscriberPayload {
            topic: "/namespace/topic".into(),
            retention_policy: 0,
            operations: vec![
                Operation::Map("/namespace/map1".into()),
                Operation::Filter("/namespace/filter1".into()),
                Operation::Map("/namespace/map2".into()),
            ],
        };
        pipe.add_subscriber(hash1, payload1, tx1.clone())
            .await
            .unwrap();

        let hash2 = "127.0.0.1:40010:1";
        let (tx2, _) = mpsc::unbounded();
        let payload2 = SubscriberPayload {
            topic: "/namespace/topic".into(),
            retention_policy: 0,
            operations: vec![
                Operation::Map("/namespace/map1".into()),
                Operation::Filter("/namespace/filter2".into()),
                Operation::Map("/namespace/map2".into()),
            ],
        };
        pipe.add_subscriber(hash2, payload2, tx2.clone())
            .await
            .unwrap();

        let hash3 = "127.0.0.1:40011:1";
        let (tx3, _) = mpsc::unbounded();
        let payload3 = SubscriberPayload {
            topic: "/namespace/topic".into(),
            retention_policy: 0,
            operations: vec![
                Operation::Map("/namespace/map1".into()),
                Operation::Filter("/namespace/filter2".into()),
                Operation::Map("/namespace/map2".into()),
            ],
        };
        pipe.add_subscriber(hash3, payload3, tx3.clone())
            .await
            .unwrap();

        let topic_key = hash_key("/namespace/topic", "", None);
        let sub1_key = hash_key("127.0.0.1:40009:1", "right", None);
        let sub2_key = hash_key("127.0.0.1:40010:1", "right", None);
        let sub3_key = hash_key("127.0.0.1:40011:1", "right", None);
        let map1_key = hash_key("/namespace/map1", "right", Some(topic_key));
        let filter1_key = hash_key("/namespace/filter1", "right", Some(map1_key));
        let filter2_key = hash_key("/namespace/filter2", "right", Some(map1_key));
        let map21_key = hash_key("/namespace/map2", "right", Some(filter1_key));
        let map22_key = hash_key("/namespace/map2", "right", Some(filter2_key));

        assert_eq!(
            *pipe.graph.get(topic_key).unwrap(),
            Node::Root(
                Arc::new(PipelineNode::Topic("/namespace/topic".into())),
                NextHop::Hop(map1_key),
                NextHop::None
            )
        );

        assert_eq!(
            *pipe.graph.get(map1_key).unwrap(),
            Node::Right(
                Arc::new(PipelineNode::Wasm("/namespace/map1".into())),
                NextHop::MultiHop(vec![filter1_key, filter2_key]),
                topic_key,
            )
        );

        assert_eq!(
            *pipe.graph.get(filter1_key).unwrap(),
            Node::Right(
                Arc::new(PipelineNode::Wasm("/namespace/filter1".into())),
                NextHop::Hop(map21_key),
                map1_key,
            )
        );

        assert_eq!(
            *pipe.graph.get(filter2_key).unwrap(),
            Node::Right(
                Arc::new(PipelineNode::Wasm("/namespace/filter2".into())),
                NextHop::Hop(map22_key),
                map1_key,
            )
        );

        assert_eq!(
            *pipe.graph.get(map21_key).unwrap(),
            Node::Right(
                Arc::new(PipelineNode::Wasm("/namespace/map2".into())),
                NextHop::Hop(sub1_key),
                filter1_key,
            )
        );

        assert_eq!(
            *pipe.graph.get(map22_key).unwrap(),
            Node::Right(
                Arc::new(PipelineNode::Wasm("/namespace/map2".into())),
                NextHop::MultiHop(vec![sub2_key, sub3_key]),
                filter2_key,
            )
        );

        assert_eq!(
            *pipe.graph.get(sub1_key).unwrap(),
            Node::RightLeaf(
                Arc::new(PipelineNode::Subscriber(
                    "127.0.0.1:40009:1".to_owned(),
                    tx1
                )),
                map21_key
            ),
        );

        assert_eq!(
            *pipe.graph.get(sub2_key).unwrap(),
            Node::RightLeaf(
                Arc::new(PipelineNode::Subscriber(
                    "127.0.0.1:40010:1".to_owned(),
                    tx2
                )),
                map22_key
            ),
        );

        assert_eq!(
            *pipe.graph.get(sub3_key).unwrap(),
            Node::RightLeaf(
                Arc::new(PipelineNode::Subscriber(
                    "127.0.0.1:40011:1".to_owned(),
                    tx3
                )),
                map22_key
            ),
        );
    }

    #[tokio::test]
    async fn test_remove_subscriber() {
        let pipe: Pipeline = Pipeline::new();

        let (tx1, _) = mpsc::unbounded();
        let addr1 = "127.0.0.1:40009:1";
        let payload1 = SubscriberPayload {
            topic: "/namespace/topic".into(),
            retention_policy: 0,
            operations: vec![
                Operation::Map("/namespace/map1".into()),
                Operation::Filter("/namespace/filter1".into()),
                Operation::Map("/namespace/map2".into()),
            ],
        };
        pipe.add_subscriber(addr1, payload1, tx1.clone())
            .await
            .unwrap();

        let topic_key = hash_key("/namespace/topic", "", None);
        let sub1_key = hash_key(addr1, "right", None);
        let map1_key = hash_key("/namespace/map1", "right", Some(topic_key));
        let filter1_key = hash_key("/namespace/filter1", "right", Some(map1_key));
        let map21_key = hash_key("/namespace/map2", "right", Some(filter1_key));

        assert_eq!(
            *pipe.graph.get(topic_key).unwrap(),
            Node::Root(
                Arc::new(PipelineNode::Topic("/namespace/topic".into())),
                NextHop::Hop(map1_key),
                NextHop::None
            )
        );

        assert_eq!(
            *pipe.graph.get(map1_key).unwrap(),
            Node::Right(
                Arc::new(PipelineNode::Wasm("/namespace/map1".into())),
                NextHop::Hop(filter1_key),
                topic_key,
            )
        );

        assert_eq!(
            *pipe.graph.get(filter1_key).unwrap(),
            Node::Right(
                Arc::new(PipelineNode::Wasm("/namespace/filter1".into())),
                NextHop::Hop(map21_key),
                map1_key,
            )
        );

        assert_eq!(
            *pipe.graph.get(sub1_key).unwrap(),
            Node::RightLeaf(
                Arc::new(PipelineNode::Subscriber(
                    "127.0.0.1:40009:1".to_owned(),
                    tx1
                )),
                map21_key
            ),
        );

        pipe.rm_subscriber(addr1).await.unwrap();

        assert_eq!(
            *pipe.graph.get(topic_key).unwrap(),
            Node::Root(
                Arc::new(PipelineNode::Topic("/namespace/topic".into())),
                NextHop::None,
                NextHop::None
            )
        );

        assert!(pipe.graph.get(map1_key).is_none());
        assert!(pipe.graph.get(filter1_key).is_none());
        assert!(pipe.graph.get(sub1_key).is_none());
    }

    #[tokio::test]
    async fn test_remove_publisher() {
        let pipe: Pipeline = Pipeline::new();

        let addr1 = "127.0.0.1:40009:1";
        let payload1 = PublisherPayload {
            topic: "/namespace/topic".into(),
            retention_policy: 0,
            operations: vec![
                Operation::Map("/namespace/map1".into()),
                Operation::Filter("/namespace/filter1".into()),
                Operation::Map("/namespace/map2".into()),
            ],
        };
        pipe.add_publisher(addr1, payload1).await.unwrap();

        let topic_key = hash_key("/namespace/topic", "", None);
        let pub1_key = hash_key(addr1, "left", None);
        let map2_key = hash_key("/namespace/map2", "left", Some(topic_key));
        let filter1_key = hash_key("/namespace/filter1", "left", Some(map2_key));
        let map1_key = hash_key("/namespace/map1", "left", Some(filter1_key));

        assert_eq!(
            *pipe.graph.get(pub1_key).unwrap(),
            Node::LeftLeaf(
                Arc::new(PipelineNode::Publisher(addr1.to_owned())),
                map1_key
            )
        );

        assert_eq!(
            *pipe.graph.get(filter1_key).unwrap(),
            Node::Left(
                Arc::new(PipelineNode::Wasm("/namespace/filter1".into())),
                map2_key,
                NextHop::Hop(map1_key),
            )
        );

        assert_eq!(
            *pipe.graph.get(map2_key).unwrap(),
            Node::Left(
                Arc::new(PipelineNode::Wasm("/namespace/map2".into())),
                topic_key,
                NextHop::Hop(filter1_key),
            )
        );

        assert_eq!(
            *pipe.graph.get(topic_key).unwrap(),
            Node::Root(
                Arc::new(PipelineNode::Topic("/namespace/topic".into())),
                NextHop::None,
                NextHop::Hop(map2_key)
            )
        );

        pipe.rm_publisher(addr1).await.unwrap();

        assert!(pipe.graph.get(pub1_key).is_none());
        assert!(pipe.graph.get(map1_key).is_none());
        assert!(pipe.graph.get(filter1_key).is_none());
        assert!(pipe.graph.get(map2_key).is_none());
    }
}
