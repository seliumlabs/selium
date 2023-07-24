use std::collections::HashMap;
use std::{pin::Pin, sync::Arc};

use dashmap::DashMap;
use futures::{future, Future, FutureExt};
use hmac_sha512::Hash;
use log::error;

type SHA512 = [u8; 64];

#[derive(Clone, Debug, PartialEq)]
pub enum NextHop {
    Hop(SHA512),
    MultiHop(Vec<SHA512>),
    None,
}

#[derive(Debug, PartialEq)]
pub enum Node<V> {
    // Value, Next Node, Previous Node
    Root(Arc<V>, NextHop, NextHop),
    Left(Arc<V>, SHA512, NextHop),
    Right(Arc<V>, NextHop, SHA512),
    // Value, Next Node
    LeftLeaf(Arc<V>, SHA512),
    // Value, Previous Node
    RightLeaf(Arc<V>, SHA512),
}

#[derive(Debug)]
pub struct DoubleEndedTree<V> {
    inner: Arc<DashMap<SHA512, Node<V>>>,
}

impl NextHop {
    pub fn merge(self, other: Self) -> Self {
        match self {
            NextHop::Hop(k) => match other {
                NextHop::Hop(ko) => NextHop::MultiHop(vec![k, ko]),
                NextHop::MultiHop(mut ko) => {
                    ko.insert(0, k);
                    NextHop::MultiHop(ko)
                }
                NextHop::None => NextHop::Hop(k),
            },
            NextHop::MultiHop(mut k) => match other {
                NextHop::Hop(ko) => {
                    k.push(ko);
                    NextHop::MultiHop(k)
                }
                NextHop::MultiHop(mut ko) => {
                    k.append(&mut ko);
                    NextHop::MultiHop(k)
                }
                NextHop::None => NextHop::MultiHop(k),
            },
            NextHop::None => other,
        }
    }

    pub fn remove(self, key: SHA512) -> Self {
        match self {
            NextHop::Hop(_) => NextHop::None,
            NextHop::MultiHop(mut hops) => {
                hops.retain(|&x| x != key);
                if hops.len() == 1 {
                    NextHop::Hop(hops[0])
                } else {
                    NextHop::MultiHop(hops)
                }
            }
            NextHop::None => NextHop::None,
        }
    }
}

impl<V> Node<V> {
    pub fn root(value: V, next_hop: NextHop, prev_hop: NextHop) -> Self {
        Self::Root(Arc::new(value), next_hop, prev_hop)
    }

    pub fn left(value: V, next_hop: SHA512, prev_hop: NextHop) -> Self {
        Self::Left(Arc::new(value), next_hop, prev_hop)
    }

    pub fn left_leaf(value: V, next_hop: SHA512) -> Self {
        Self::LeftLeaf(Arc::new(value), next_hop)
    }

    pub fn right(value: V, next_hop: NextHop, prev_hop: SHA512) -> Self {
        Self::Right(Arc::new(value), next_hop, prev_hop)
    }

    pub fn right_leaf(value: V, prev_hop: SHA512) -> Self {
        Self::RightLeaf(Arc::new(value), prev_hop)
    }

    pub fn get_value(&self) -> Arc<V> {
        match self {
            Self::Root(v, _, _) => v.clone(),
            Self::Left(v, _, _) => v.clone(),
            Self::Right(v, _, _) => v.clone(),
            Self::LeftLeaf(v, _) => v.clone(),
            Self::RightLeaf(v, _) => v.clone(),
        }
    }

    pub fn get_next_hop(&self) -> NextHop {
        match self {
            Self::Root(_, n, _) => n.clone(),
            Self::Left(_, n, _) => NextHop::Hop(*n),
            Self::Right(_, n, _) => n.clone(),
            Self::LeftLeaf(_, n) => NextHop::Hop(*n),
            Self::RightLeaf(_, _) => NextHop::None,
        }
    }

    pub fn unpack(&self) -> (Arc<V>, NextHop) {
        (self.get_value(), self.get_next_hop())
    }
}

impl<V> DoubleEndedTree<V>
where
    V: Send + Sync + 'static,
{
    pub fn new() -> Self {
        DoubleEndedTree {
            inner: Arc::new(DashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn get<'a>(
        &'a self,
        key: SHA512,
    ) -> Option<dashmap::mapref::one::Ref<'a, SHA512, Node<V>>> {
        self.inner.get(&key)
    }

    pub fn add_root<K: AsRef<[u8]>>(&self, key: K, value: V) -> SHA512 {
        let hkey = Hash::hash(key);

        if !self.inner.contains_key(&hkey) {
            self.inner
                .insert(hkey, Node::root(value, NextHop::None, NextHop::None));
        }

        hkey
    }

    pub fn add_left<K: AsRef<[u8]>>(&self, key: K, value: V, left_of: SHA512) -> SHA512 {
        let hkey = hash_key(key, "left", Some(left_of));
        self._add_left(hkey, Node::left(value, left_of, NextHop::None), left_of);

        hkey
    }

    pub fn add_left_leaf<K: AsRef<[u8]>>(&self, key: K, value: V, left_of: SHA512) -> SHA512 {
        let hkey = hash_key(key, "left", None);
        self._add_left(hkey, Node::left_leaf(value, left_of), left_of);

        hkey
    }

    pub fn add_right<K: AsRef<[u8]>>(&self, key: K, value: V, right_of: SHA512) -> SHA512 {
        let hkey = hash_key(key, "right", Some(right_of));
        self._add_right(hkey, Node::right(value, NextHop::None, right_of), right_of);

        hkey
    }

    pub fn add_right_leaf<K: AsRef<[u8]>>(&self, key: K, value: V, right_of: SHA512) -> SHA512 {
        let hkey = hash_key(key, "right", None);
        self._add_right(hkey, Node::right_leaf(value, right_of), right_of);

        hkey
    }

    fn _rm_left_branch(&self, hashes_to_remove: HashMap<SHA512, SHA512>) {
        for (prev_hash, node_hash) in &hashes_to_remove {
            self.inner.alter(prev_hash, |_, n| match n {
                Node::Left(v, n, p) => Node::Left(v, n, p.remove(*node_hash)),
                Node::Root(v, n, p) => Node::Root(v, n, p.remove(*node_hash)),
                _ => panic!("Not supported"),
            });
            self.inner.remove(node_hash);
        }
    }

    fn _rm_left_leaf(&self, hash: SHA512) {
        let mut current_hash = hash;
        let mut hashes_to_remove = HashMap::new();
        loop {
            let node = self
                .inner
                .get(&current_hash)
                .expect("Hash is not in the graph");
            match *node {
                Node::Root(_, _, _) => break,
                Node::LeftLeaf(_, ref n) => {
                    hashes_to_remove.insert(*n, current_hash);
                    current_hash = *n;
                }
                Node::Left(_, ref n, ref p) => {
                    if matches!(p, NextHop::Hop(_)) {
                        hashes_to_remove.insert(*n, current_hash);
                        current_hash = *n;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        self._rm_left_branch(hashes_to_remove);
    }

    pub fn rm_left_leaf<K: AsRef<[u8]>>(&self, key: K) {
        let hash = hash_key(key, "left", None);
        self._rm_left_leaf(hash);
    }

    fn _rm_right_branch(&self, hashes_to_remove: HashMap<SHA512, SHA512>) {
        for (prev_hash, node_hash) in &hashes_to_remove {
            self.inner.alter(prev_hash, |_, n| match n {
                Node::Right(v, n, p) => Node::Right(v, n.remove(*node_hash), p),
                Node::Root(v, n, p) => Node::Root(v, n.remove(*node_hash), p),
                _ => panic!("Not supported"),
            });
            self.inner.remove(node_hash);
        }
    }

    pub fn _rm_right_leaf(&self, hash: SHA512) {
        let mut current_hash = hash;
        let mut hashes_to_remove = HashMap::new();
        loop {
            let node = self
                .inner
                .get(&current_hash)
                .expect("Hash is not in the graph");
            match *node {
                Node::Root(_, _, _) => break,
                Node::RightLeaf(_, ref p) => {
                    hashes_to_remove.insert(*p, current_hash);
                    current_hash = *p;
                }
                Node::Right(_, ref n, ref p) => {
                    if matches!(n, NextHop::Hop(_)) {
                        hashes_to_remove.insert(*p, current_hash);
                        current_hash = *p;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        self._rm_right_branch(hashes_to_remove);
    }

    pub fn rm_right_leaf<K: AsRef<[u8]>>(&self, key: K) {
        let hash = hash_key(key, "right", None);
        self._rm_right_leaf(hash);
    }

    pub fn fold_branches<T, F, Fut>(
        &self,
        init: T,
        start: SHA512,
        handler: F,
    ) -> Pin<Box<dyn Future<Output = T> + Send>>
    where
        F: Fn(T, Arc<V>) -> Fut + Copy + Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
        T: Clone + Send + Sync + 'static,
    {
        let inner = self.inner.clone();
        _fold(inner, init, start, handler)
    }

    fn _add_left(&self, key: SHA512, value: Node<V>, left_of: SHA512) {
        if !self.inner.contains_key(&key) {
            self.inner.insert(key, value);

            self.inner.alter(&left_of, |_, node| match node {
                Node::Root(v, n, p) => Node::Root(v, n, p.merge(NextHop::Hop(key))),
                Node::Left(v, n, p) => Node::Left(v, n, p.merge(NextHop::Hop(key))),
                Node::Right(_, _, _) | Node::RightLeaf(_, _) | Node::LeftLeaf(_, _) => {
                    panic!("Left nodes must be left of Node::Root or Node::Left");
                }
            });
        }
    }

    fn _add_right(&self, key: SHA512, value: Node<V>, right_of: SHA512) {
        if !self.inner.contains_key(&key) {
            self.inner.insert(key, value);

            self.inner.alter(&right_of, |_, node| match node {
                Node::Root(v, n, p) => Node::Root(v, n.merge(NextHop::Hop(key)), p),
                Node::Right(v, n, p) => Node::Right(v, n.merge(NextHop::Hop(key)), p),
                Node::Left(_, _, _) | Node::LeftLeaf(_, _) | Node::RightLeaf(_, _) => {
                    panic!("Right nodes must be right of Node::Root or Node::Right");
                }
            });
        }
    }
}

impl<V> Clone for DoubleEndedTree<V> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

fn _fold<V, T, F, Fut>(
    inner: Arc<DashMap<SHA512, Node<V>>>,
    init: T,
    mut start: SHA512,
    handler: F,
) -> Pin<Box<dyn Future<Output = T> + Send>>
where
    F: Fn(T, Arc<V>) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = T> + Send + 'static,
    T: Clone + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    let mut fut = future::ready(init).boxed();
    loop {
        let node = match inner.get(&start) {
            Some(n) => n,
            None => {
                error!("Invalid next hop: {:?}", start);
                return fut;
            }
        };

        let (value, next_hop) = node.unpack();

        fut = fut.then(move |i| handler(i, value)).boxed();

        match next_hop {
            NextHop::Hop(h) => start = h,
            NextHop::MultiHop(m) => {
                fut = fut.shared().boxed();
                for hop in m {
                    let inner_c = inner.clone();
                    fut = fut.then(move |i| _fold(inner_c, i, hop, handler)).boxed();
                }
                break fut;
            }
            NextHop::None => break fut,
        }
    }
}

pub fn hash_key<K: AsRef<[u8]>>(key: K, suffix: &str, concat: Option<SHA512>) -> SHA512 {
    let mut k = match concat {
        Some(c) => {
            let mut v = c.to_vec();
            v.extend_from_slice(key.as_ref());
            v
        }
        None => key.as_ref().to_vec(),
    };
    k.extend_from_slice(suffix.as_bytes());
    Hash::hash(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_root_idempotent() {
        let p = DoubleEndedTree::new();
        let key = "key".to_owned();
        p.add_root(key.clone(), ());
        assert_eq!(p.inner.len(), 1);

        p.add_root(key, ());
        assert_eq!(p.inner.len(), 1);
    }

    #[tokio::test]
    async fn test_fold_branches() {
        let p = DoubleEndedTree::new();
        let root = p.add_root("root", 2);
        let left1 = p.add_left("left1", 1, root);
        let left_leaf = p.add_left_leaf("left2", 0, left1);
        p.add_left("unrelated_left", 5, root);
        let right1 = p.add_right("right1", 3, root);
        p.add_right_leaf("right2", 4, right1);
        p.add_right_leaf("right3", 5, right1);
        p.add_right_leaf("right4", 6, right1);

        let x = p
            .fold_branches(0, left_leaf, |base, value| {
                future::ready(base + value.as_ref())
            })
            .await;
        assert_eq!(x, 21);
    }

    #[tokio::test]
    async fn test_remove_left() {
        let p = DoubleEndedTree::new();
        let root = p.add_root("root", 2);
        let left1 = p.add_left("left1", 1, root);
        let left_leaf = p.add_left_leaf("left2", 0, left1);

        p.rm_left_leaf("left2");

        let result = p.get(left_leaf);
        assert!(result.is_none());

        let result = p.get(left1);
        assert!(result.is_none());

        let root_node = p.get(root).unwrap();
        match root_node.value() {
            Node::Root(_, _, p) => assert!(matches!(p, _)),
            _ => {}
        };
    }

    #[tokio::test]
    async fn test_remove_left2() {
        let p = DoubleEndedTree::new();
        let root = p.add_root("root", 5);

        let left1 = p.add_left("left1", 4, root);
        let left1_leaf = p.add_left_leaf("left1_leaf", 3, left1);

        let left2_leaf = p.add_left_leaf("left2_leaf", 2, left1);

        p.rm_left_leaf("left2_leaf");

        let result = p.get(left2_leaf);
        assert!(result.is_none());

        // The left1 branch should still exist
        let result = p.get(left1);
        assert!(result.is_some());
        match result.unwrap().value() {
            Node::Left(_, _, p) => {
                assert!(matches!(p, NextHop::Hop(_)))
            }
            _ => assert!(false),
        }

        let result = p.get(left1_leaf);
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_remove_right() {
        let p = DoubleEndedTree::new();
        let root = p.add_root("root", 2);
        let right1 = p.add_right("right1", 1, root);
        let right_leaf = p.add_right_leaf("right2", 0, right1);

        println!("rm_right_leaf");
        p.rm_right_leaf("right2");

        let result = p.get(right_leaf);
        assert!(result.is_none());

        let result = p.get(right1);
        assert!(result.is_none());

        let root_node = p.get(root).unwrap();
        match root_node.value() {
            Node::Root(_, n, _) => assert!(matches!(n, NextHop::None)),
            _ => assert!(false),
        };
    }
}
