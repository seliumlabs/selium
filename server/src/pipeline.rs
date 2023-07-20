use std::net::SocketAddr;

use anyhow::Result;
use futures::channel::mpsc::UnboundedSender;
use selium::protocol::{PublisherPayload, SubscriberPayload};

use crate::graph::{hash_key, PipeGraph};

#[derive(Debug)]
enum PipelineNode {
    Publisher,
    Subscriber(SocketAddr, UnboundedSender<String>),
    Topic(String),
    // @TODO - WASM Support
    // Wasm(String),
}

#[derive(Debug)]
pub struct Pipeline {
    graph: PipeGraph<PipelineNode>,
}

impl ToString for PipelineNode {
    fn to_string(&self) -> String {
        match self {
            Self::Publisher => "Publisher".into(),
            Self::Subscriber(_, _) => "Subscriber".into(),
            Self::Topic(_) => "Topic".into(),
            // @TODO - WASM Support
            // Self::Wasm(s) => format!("WASM ({s})"),
        }
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
            graph: PipeGraph::new(),
        }
    }

    pub fn add_publisher(&self, addr: SocketAddr, payload: PublisherPayload) {
        // First add the topic
        let left_of = self
            .graph
            .add_root(payload.topic.clone(), PipelineNode::Topic(payload.topic));

        // Now iterate backwards up the pipe operations towards the socket.
        // We do this so that we can set the next hop.
        // @TODO - WASM Support
        // for op in payload.operations.into_iter().rev() {
        //     let key = match op {
        //         Operation::Filter(f) => f,
        //         Operation::Map(m) => m,
        //     };

        //     left_of = self
        //         .graph
        //         .add_left(key.clone(), PipelineNode::Wasm(key), left_of);
        // }

        // Finally, add the publisher
        self.graph
            .add_left_leaf(addr.to_string(), PipelineNode::Publisher, left_of);
    }

    pub fn add_subscriber(
        &self,
        addr: SocketAddr,
        payload: SubscriberPayload,
        sock: UnboundedSender<String>,
    ) {
        // First add the topic
        let right_of = self
            .graph
            .add_root(payload.topic.clone(), PipelineNode::Topic(payload.topic));

        // Now iterate over the pipe operations towards the socket
        // @TODO - WASM Support
        // for op in payload.operations.into_iter() {
        //     let key = match op {
        //         Operation::Filter(f) => f,
        //         Operation::Map(m) => m,
        //     };

        //     right_of = self
        //         .graph
        //         .add_right(key.clone(), PipelineNode::Wasm(key), right_of);
        // }

        // Finally, add the subscriber
        self.graph.add_right_leaf(
            addr.to_string(),
            PipelineNode::Subscriber(addr, sock),
            right_of,
        );
    }

    pub fn rm_publisher(&self, _addr: SocketAddr) {
        unimplemented!();
    }

    pub fn rm_subscriber(&self, _addr: SocketAddr) {
        unimplemented!();
    }

    pub fn traverse(&self, publisher: SocketAddr, message: String) -> Result<String> {
        let key = hash_key(publisher.to_string(), "left", None);
        self.graph.fold(message, key, |msg, node| {
            match node {
                PipelineNode::Publisher | PipelineNode::Topic(_) => (),
                PipelineNode::Subscriber(_, sock) => {
                    sock.unbounded_send(msg.clone())?;
                }
                // @TODO - WASM Support
                // PipelineNode::Wasm(w) => msg += w,
            };

            Ok(msg)
        })
    }
}

#[cfg(test)]
mod tests {
    use futures::channel::mpsc;

    use super::*;
    use crate::graph::{hash_key, MultiHop, Node};
    use std::str::FromStr;

    #[test]
    fn test_add_publisher() {
        let pipe: Pipeline = Pipeline::new();

        let addr1 = SocketAddr::from_str("127.0.0.1:40009").unwrap();
        let payload1 = PublisherPayload {
            topic: "/namespace/topic".into(),
            retention_policy: 0,
            // @TODO - WASM Support
            // operations: vec![
            //     Operation::Map("/namespace/map1".into()),
            //     Operation::Filter("/namespace/filter1".into()),
            //     Operation::Map("/namespace/map2".into()),
            // ],
        };
        pipe.add_publisher(addr1, payload1);

        let addr2 = SocketAddr::from_str("127.0.0.1:40010").unwrap();
        let payload2 = PublisherPayload {
            topic: "/namespace/topic".into(),
            retention_policy: 0,
            // @TODO - WASM Support
            // operations: vec![
            //     Operation::Map("/namespace/map1".into()),
            //     Operation::Filter("/namespace/filter2".into()),
            //     Operation::Map("/namespace/map2".into()),
            // ],
        };
        pipe.add_publisher(addr2, payload2);

        let addr3 = SocketAddr::from_str("127.0.0.1:40011").unwrap();
        let payload3 = PublisherPayload {
            topic: "/namespace/topic".into(),
            retention_policy: 0,
            // @TODO - WASM Support
            // operations: vec![
            //     Operation::Map("/namespace/map1".into()),
            //     Operation::Filter("/namespace/filter2".into()),
            //     Operation::Map("/namespace/map2".into()),
            // ],
        };
        pipe.add_publisher(addr3, payload3);

        let topic_key = hash_key("/namespace/topic", "", None);
        let pub1_key = hash_key("127.0.0.1:40009", "left", None);
        let pub2_key = hash_key("127.0.0.1:40010", "left", None);
        let pub3_key = hash_key("127.0.0.1:40011", "left", None);
        // @TODO - WASM Support
        // let map2_key = hash_key("/namespace/map2", "left", Some(topic_key));
        // let filter1_key = hash_key("/namespace/filter1", "left", Some(map2_key));
        // let filter2_key = hash_key("/namespace/filter2", "left", Some(map2_key));
        // let map11_key = hash_key("/namespace/map1", "left", Some(filter1_key));
        // let map12_key = hash_key("/namespace/map1", "left", Some(filter2_key));

        assert_eq!(
            *pipe.graph.get(pub1_key).unwrap(),
            // @TODO - WASM Support
            // Node::LeftLeaf(PipelineNode::Publisher, map11_key)
            Node::LeftLeaf(PipelineNode::Publisher, topic_key)
        );

        assert_eq!(
            *pipe.graph.get(pub2_key).unwrap(),
            // @TODO - WASM Support
            // Node::LeftLeaf(PipelineNode::Publisher, map12_key)
            Node::LeftLeaf(PipelineNode::Publisher, topic_key)
        );

        assert_eq!(
            *pipe.graph.get(pub3_key).unwrap(),
            // @TODO - WASM Support
            // Node::LeftLeaf(PipelineNode::Publisher, map12_key)
            Node::LeftLeaf(PipelineNode::Publisher, topic_key)
        );

        // @TODO - WASM Support
        // assert_eq!(
        //     *pipe.graph.get(map11_key).unwrap(),
        //     Node::Left(
        //         PipelineNode::Wasm("/namespace/map1".into()),
        //         filter1_key,
        //         MultiHop::Hop(pub1_key),
        //     )
        // );

        // assert_eq!(
        //     *pipe.graph.get(map12_key).unwrap(),
        //     Node::Left(
        //         PipelineNode::Wasm("/namespace/map1".into()),
        //         filter2_key,
        //         MultiHop::MultiHop(vec![pub2_key, pub3_key]),
        //     )
        // );

        // assert_eq!(
        //     *pipe.graph.get(filter1_key).unwrap(),
        //     Node::Left(
        //         PipelineNode::Wasm("/namespace/filter1".into()),
        //         map2_key,
        //         MultiHop::Hop(map11_key),
        //     )
        // );

        // assert_eq!(
        //     *pipe.graph.get(filter2_key).unwrap(),
        //     Node::Left(
        //         PipelineNode::Wasm("/namespace/filter2".into()),
        //         map2_key,
        //         MultiHop::Hop(map12_key),
        //     )
        // );

        // assert_eq!(
        //     *pipe.graph.get(map2_key).unwrap(),
        //     Node::Left(
        //         PipelineNode::Wasm("/namespace/map2".into()),
        //         topic_key,
        //         MultiHop::MultiHop(vec![filter1_key, filter2_key]),
        //     )
        // );

        assert_eq!(
            *pipe.graph.get(topic_key).unwrap(),
            Node::Root(
                PipelineNode::Topic("/namespace/topic".into()),
                MultiHop::None,
                // @TODO - WASM Support
                // MultiHop::Hop(map2_key)
                MultiHop::MultiHop(vec![pub1_key, pub2_key, pub3_key])
            )
        );
    }

    #[test]
    fn test_add_subscriber() {
        let pipe: Pipeline = Pipeline::new();

        let addr1 = SocketAddr::from_str("127.0.0.1:40009").unwrap();
        let (tx1, _) = mpsc::unbounded();
        let payload1 = SubscriberPayload {
            topic: "/namespace/topic".into(),
            // @TODO - WASM Support
            // operations: vec![
            //     Operation::Map("/namespace/map1".into()),
            //     Operation::Filter("/namespace/filter1".into()),
            //     Operation::Map("/namespace/map2".into()),
            // ],
        };
        pipe.add_subscriber(addr1, payload1, tx1.clone());

        let addr2 = SocketAddr::from_str("127.0.0.1:40010").unwrap();
        let (tx2, _) = mpsc::unbounded();
        let payload2 = SubscriberPayload {
            topic: "/namespace/topic".into(),
            // @TODO - WASM Support
            // operations: vec![
            //     Operation::Map("/namespace/map1".into()),
            //     Operation::Filter("/namespace/filter2".into()),
            //     Operation::Map("/namespace/map2".into()),
            // ],
        };
        pipe.add_subscriber(addr2, payload2, tx2.clone());

        let addr3 = SocketAddr::from_str("127.0.0.1:40011").unwrap();
        let (tx3, _) = mpsc::unbounded();
        let payload3 = SubscriberPayload {
            topic: "/namespace/topic".into(),
            // @TODO - WASM Support
            // operations: vec![
            //     Operation::Map("/namespace/map1".into()),
            //     Operation::Filter("/namespace/filter2".into()),
            //     Operation::Map("/namespace/map2".into()),
            // ],
        };
        pipe.add_subscriber(addr3, payload3, tx3.clone());

        let topic_key = hash_key("/namespace/topic", "", None);
        let sub1_key = hash_key("127.0.0.1:40009", "right", None);
        let sub2_key = hash_key("127.0.0.1:40010", "right", None);
        let sub3_key = hash_key("127.0.0.1:40011", "right", None);
        // @TODO - WASM Support
        // let map1_key = hash_key("/namespace/map1", "right", Some(topic_key));
        // let filter1_key = hash_key("/namespace/filter1", "right", Some(map1_key));
        // let filter2_key = hash_key("/namespace/filter2", "right", Some(map1_key));
        // let map21_key = hash_key("/namespace/map2", "right", Some(filter1_key));
        // let map22_key = hash_key("/namespace/map2", "right", Some(filter2_key));

        assert_eq!(
            *pipe.graph.get(topic_key).unwrap(),
            Node::Root(
                PipelineNode::Topic("/namespace/topic".into()),
                // @TODO - WASM Support
                // MultiHop::Hop(map1_key),
                MultiHop::MultiHop(vec![sub1_key, sub2_key, sub3_key]),
                MultiHop::None
            )
        );

        // @TODO - WASM Support
        // assert_eq!(
        //     *pipe.graph.get(map1_key).unwrap(),
        //     Node::Right(
        //         PipelineNode::Wasm("/namespace/map1".into()),
        //         MultiHop::MultiHop(vec![filter1_key, filter2_key]),
        //         topic_key,
        //     )
        // );

        // assert_eq!(
        //     *pipe.graph.get(filter1_key).unwrap(),
        //     Node::Right(
        //         PipelineNode::Wasm("/namespace/filter1".into()),
        //         MultiHop::Hop(map21_key),
        //         map1_key,
        //     )
        // );

        // assert_eq!(
        //     *pipe.graph.get(filter2_key).unwrap(),
        //     Node::Right(
        //         PipelineNode::Wasm("/namespace/filter2".into()),
        //         MultiHop::Hop(map22_key),
        //         map1_key,
        //     )
        // );

        // assert_eq!(
        //     *pipe.graph.get(map21_key).unwrap(),
        //     Node::Right(
        //         PipelineNode::Wasm("/namespace/map2".into()),
        //         MultiHop::Hop(sub1_key),
        //         filter1_key,
        //     )
        // );

        // assert_eq!(
        //     *pipe.graph.get(map22_key).unwrap(),
        //     Node::Right(
        //         PipelineNode::Wasm("/namespace/map2".into()),
        //         MultiHop::MultiHop(vec![sub2_key, sub3_key]),
        //         filter2_key,
        //     )
        // );

        assert_eq!(
            *pipe.graph.get(sub1_key).unwrap(),
            Node::RightLeaf(
                PipelineNode::Subscriber(SocketAddr::from_str("127.0.0.1:40009").unwrap(), tx1),
                // @TODO - WASM Support
                // map21_key
                topic_key
            ),
        );

        assert_eq!(
            *pipe.graph.get(sub2_key).unwrap(),
            Node::RightLeaf(
                PipelineNode::Subscriber(SocketAddr::from_str("127.0.0.1:40010").unwrap(), tx2),
                // @TODO - WASM Support
                // map22_key
                topic_key
            ),
        );

        assert_eq!(
            *pipe.graph.get(sub3_key).unwrap(),
            Node::RightLeaf(
                PipelineNode::Subscriber(SocketAddr::from_str("127.0.0.1:40011").unwrap(), tx3),
                // @TODO - WASM Support
                // map22_key
                topic_key
            ),
        );
    }
}
