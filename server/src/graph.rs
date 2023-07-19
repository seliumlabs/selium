use anyhow::{anyhow, Result};
use dashmap::DashMap;
use hmac_sha512::Hash;

type SHA512 = [u8; 64];

#[derive(Debug, PartialEq)]
pub enum MultiHop {
    Hop(SHA512),
    MultiHop(Vec<SHA512>),
    None,
}

#[derive(Debug, PartialEq)]
pub enum Node<V> {
    // Value, Next Node, Previous Node
    Root(V, MultiHop, MultiHop),
    Left(V, SHA512, MultiHop),
    Right(V, MultiHop, SHA512),
    // Value, Next Node
    LeftLeaf(V, SHA512),
    // Value, Previous Node
    RightLeaf(V, SHA512),
}

#[derive(Debug)]
pub struct PipeGraph<V> {
    inner: DashMap<SHA512, Node<V>>,
}

impl MultiHop {
    pub fn merge(self, other: Self) -> Self {
        match self {
            MultiHop::Hop(k) => match other {
                MultiHop::Hop(ko) => MultiHop::MultiHop(vec![k, ko]),
                MultiHop::MultiHop(mut ko) => {
                    ko.insert(0, k);
                    MultiHop::MultiHop(ko)
                }
                MultiHop::None => MultiHop::Hop(k),
            },
            MultiHop::MultiHop(mut k) => match other {
                MultiHop::Hop(ko) => {
                    k.push(ko);
                    MultiHop::MultiHop(k)
                }
                MultiHop::MultiHop(mut ko) => {
                    k.append(&mut ko);
                    MultiHop::MultiHop(k)
                }
                MultiHop::None => MultiHop::MultiHop(k),
            },
            MultiHop::None => other,
        }
    }
}

impl<V> Node<V> {
    pub fn root(value: V, next_hop: MultiHop, prev_hop: MultiHop) -> Self {
        Self::Root(value, next_hop, prev_hop)
    }

    pub fn left(value: V, next_hop: SHA512, prev_hop: MultiHop) -> Self {
        Self::Left(value, next_hop, prev_hop)
    }

    pub fn left_leaf(value: V, next_hop: SHA512) -> Self {
        Self::LeftLeaf(value, next_hop)
    }

    pub fn right(value: V, next_hop: MultiHop, prev_hop: SHA512) -> Self {
        Self::Right(value, next_hop, prev_hop)
    }

    pub fn right_leaf(value: V, prev_hop: SHA512) -> Self {
        Self::RightLeaf(value, prev_hop)
    }

    pub fn get_value(&self) -> &V {
        match self {
            Self::Root(v, _, _) => v,
            Self::Left(v, _, _) => v,
            Self::Right(v, _, _) => v,
            Self::LeftLeaf(v, _) => v,
            Self::RightLeaf(v, _) => v,
        }
    }
}

impl<V> PipeGraph<V> {
    pub fn new() -> Self {
        PipeGraph {
            inner: DashMap::new(),
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
                .insert(hkey, Node::root(value, MultiHop::None, MultiHop::None));
        }

        hkey
    }

    pub fn add_left<K: AsRef<[u8]>>(&self, key: K, value: V, left_of: SHA512) -> SHA512 {
        let hkey = hash_key(key, "left", Some(left_of));
        self._add_left(hkey, Node::left(value, left_of, MultiHop::None), left_of);

        hkey
    }

    pub fn add_left_leaf<K: AsRef<[u8]>>(&self, key: K, value: V, left_of: SHA512) -> SHA512 {
        let hkey = hash_key(key, "left", None);
        self._add_left(hkey, Node::left_leaf(value, left_of), left_of);

        hkey
    }

    pub fn add_right<K: AsRef<[u8]>>(&self, key: K, value: V, right_of: SHA512) -> SHA512 {
        let hkey = hash_key(key, "right", Some(right_of));
        self._add_right(hkey, Node::right(value, MultiHop::None, right_of), right_of);

        hkey
    }

    pub fn add_right_leaf<K: AsRef<[u8]>>(&self, key: K, value: V, right_of: SHA512) -> SHA512 {
        let hkey = hash_key(key, "right", None);
        self._add_right(hkey, Node::right_leaf(value, right_of), right_of);

        hkey
    }

    pub fn rm_left(_hash: SHA512) {
        unimplemented!();
    }

    pub fn rm_right(_hash: SHA512) {
        unimplemented!();
    }

    pub fn fold<F, T>(&self, mut init: T, start: SHA512, handler: F) -> Result<T>
    where
        F: Fn(T, &V) -> Result<T>,
    {
        let mut node = self
            .inner
            .get(&start)
            .ok_or(anyhow!("Invalid next hop: {:?}", start))?;

        loop {
            init = handler(init, node.get_value())?;

            match *node {
                Node::Left(_, ref hop, _) | Node::LeftLeaf(_, ref hop) => {
                    node = self
                        .inner
                        .get(hop)
                        .ok_or(anyhow!("Invalid next hop: {:?}", hop))?
                }
                Node::Right(_, MultiHop::Hop(ref hop), _)
                | Node::Root(_, MultiHop::Hop(ref hop), _) => {
                    node = self
                        .inner
                        .get(hop)
                        .ok_or(anyhow!("Invalid next hop: {:?}", hop))?
                }
                Node::Right(_, MultiHop::MultiHop(ref _multi), _)
                | Node::Root(_, MultiHop::MultiHop(ref _multi), _) => unimplemented!(),
                _ => break,
            }
        }

        Ok(init)
    }

    fn _add_left(&self, key: SHA512, value: Node<V>, left_of: SHA512) {
        if !self.inner.contains_key(&key) {
            self.inner.insert(key, value);

            self.inner.alter(&left_of, |_, node| match node {
                Node::Root(v, n, p) => Node::Root(v, n, p.merge(MultiHop::Hop(key))),
                Node::Left(v, n, p) => Node::Left(v, n, p.merge(MultiHop::Hop(key))),
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
                Node::Root(v, n, p) => Node::Root(v, n.merge(MultiHop::Hop(key)), p),
                Node::Right(v, n, p) => Node::Right(v, n.merge(MultiHop::Hop(key)), p),
                Node::Left(_, _, _) | Node::LeftLeaf(_, _) | Node::RightLeaf(_, _) => {
                    panic!("Right nodes must be right of Node::Root or Node::Right");
                }
            });
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
        let p = PipeGraph::new();
        let key = "key".to_owned();
        p.add_root(key.clone(), ());
        assert_eq!(p.inner.len(), 1);

        p.add_root(key, ());
        assert_eq!(p.inner.len(), 1);
    }

    #[test]
    fn test_fold() {
        let p = PipeGraph::new();
        let root = p.add_root("root", 2);
        let left1 = p.add_left("left1", 1, root);
        let left_leaf = p.add_left_leaf("left2", 0, left1);
        // p.add_left_idempotent("unrelated_left", 5, "root");
        let right1 = p.add_right("right1", 3, root);
        p.add_right_leaf("right2", 4, right1);
        // p.add_right_idempotent("unrelated_right", 5, "right1");

        let x = p
            .fold(0, left_leaf, |base, value| Ok(base + value))
            .unwrap();
        assert_eq!(x, 10);
    }
}
