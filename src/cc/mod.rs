use graph::{Graph, NodeIndex};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt::Debug;
use std::hash::Hash;
use unify::{UnifyKey, UnificationTable};

#[cfg(test)]
mod test;

pub struct CongruenceClosure<K: Hash + Eq> {
    map: HashMap<K, Token>,
    table: UnificationTable<Token>,
    graph: Graph<K, ()>,
}

pub trait Key : Hash + Eq + Clone + Debug {
    fn shallow_eq(&self, key: &Self) -> bool;
    fn successors(&self) -> Vec<Self>;
}

#[derive(Copy,Clone,Debug,PartialEq)]
pub struct Token {
    // this is the index both for the graph and the unification table,
    // since for every node there is also a slot in the unification
    // table
    index: u32,
}

impl Token {
    fn new(index: u32) -> Token {
        Token { index: index }
    }

    fn from_node(node: NodeIndex) -> Token {
        Token { index: node.0 as u32 }
    }

    fn node(&self) -> NodeIndex {
        NodeIndex(self.index as usize)
    }
}

impl UnifyKey for Token {
    type Value = ();
    fn index(&self) -> u32 {
        self.index
    }
    fn from_index(i: u32) -> Token {
        Token::new(i)
    }
    fn tag() -> &'static str {
        "CongruenceClosure"
    }
}


impl<K: Key> CongruenceClosure<K> {
    pub fn new() -> CongruenceClosure<K> {
        CongruenceClosure {
            map: HashMap::new(),
            table: UnificationTable::new(),
            graph: Graph::new(),
        }
    }

    pub fn add(&mut self, key: K) -> Token {
        debug!("add(): key={:?}", key);

        let (is_new, token) = self.new_token(&key);
        debug!("add: key={:?} is_new={:?} token={:?}", key, is_new, token);

        // if this node is already in the graph, we are done
        if !is_new {
            return token;
        }

        // Otherwise, we want to add the 'successors' also. So, for
        // example, if we are adding `Box<Foo>`, the successor would
        // be `Foo`.  So go ahead and recursively add `Foo` if it
        // doesn't already exist.
        let successors: Vec<Token> = key.successors()
                                        .into_iter()
                                        .map(|s| self.add(s))
                                        .collect();

        debug!("add: key={:?} successors={:?}", key, successors);

        // Now we have to be a bit careful. It might be that we are
        // adding `Box<Foo>`, but `Foo` was already present, and in
        // fact equated with `Bar`. That is, maybe we had a graph like:
        //
        //      Box<Bar> -> Bar == Foo
        //
        // Now we just added `Box<Foo>`, but we need to equate
        // `Box<Foo>` and `Box<Bar>`.
        for successor in successors {
            // get set of predecessors for each successor BEFORE we add the new node;
            // this would be `Box<Bar>` in the above example.
            let predecessors: Vec<_> = self.graph.predecessor_nodes(token.node()).collect();

            debug!("add: key={:?} successor={:?} predecessors={:?}",
                   key,
                   successor,
                   predecessors);

            // add edge from new node `Box<Foo>` to its successor `Foo`
            self.graph.add_edge(token.node(), successor.node(), ());

            // Now we have to consider merging the old predecessors,
            // like `Box<Bar>`, with this new node `Box<Foo>`.
            //
            // Note that in other cases it might be that no merge will
            // occur. For example, if we were adding `(A1, B1)` to a
            // graph like this:
            //
            //     (A, B) -> A == A1
            //        |
            //        v
            //        B
            //
            // In this case, the predecessor would be `(A, B)`; but we don't
            // know that `B == B1`, so we can't merge that with `(A1, B1)`.
            for predecessor in predecessors {
                self.algorithm().maybe_merge(token, Token::from_node(predecessor));
            }
        }

        token
    }

    pub fn merge(&mut self, key1: K, key2: K) {
        let token1 = self.add(key1);
        let token2 = self.add(key2);
        self.algorithm().merge(token1, token2);
    }

    pub fn merged(&mut self, key1: K, key2: K) -> bool {
        // Sadly, even if `key1` and `key2` are not yet in the map,
        // they might be unioned, because some of their successors
        // might be in the map.

        let token1 = self.add(key1);
        let token2 = self.add(key2);
        self.algorithm().unioned(token1, token2)
    }

    fn new_token(&mut self, key: &K) -> (bool, Token) {
        match self.map.entry(key.clone()) {
            Entry::Occupied(slot) => (false, slot.get().clone()),
            Entry::Vacant(slot) => {
                let token = self.table.new_key(());
                let node = self.graph.add_node(key.clone());
                assert_eq!(token.node(), node);
                slot.insert(token);
                (true, token)
            }
        }
    }

    fn algorithm(&mut self) -> Algorithm<K> {
        Algorithm {
            graph: &self.graph,
            table: &mut self.table,
        }
    }
}

struct Algorithm<'a, K: 'a> {
    graph: &'a Graph<K, ()>,
    table: &'a mut UnificationTable<Token>,
}

impl<'a, K: Key> Algorithm<'a, K> {
    fn merge(&mut self, u: Token, v: Token) {
        debug!("merge(): u={:?} v={:?}", u, v);

        if self.unioned(u, v) {
            return;
        }

        let u_preds = self.all_preds(u);
        let v_preds = self.all_preds(v);

        self.union(u, v);

        for &p_u in &u_preds {
            for &p_v in &v_preds {
                self.maybe_merge(p_u, p_v);
            }
        }
    }

    fn all_preds(&mut self, u: Token) -> Vec<Token> {
        let graph = self.graph;
        self.table
            .unioned_keys(u)
            .flat_map(|k| graph.predecessor_nodes(k.node()))
            .map(|i| Token::from_node(i))
            .collect()
    }

    fn maybe_merge(&mut self, p_u: Token, p_v: Token) {
        debug!("maybe_merge(): p_u={:?} p_v={:?}", p_u, p_v);

        if !self.unioned(p_u, p_v) && self.shallow_eq(p_u, p_v) && self.congruent(p_u, p_v) {
            self.merge(p_u, p_v);
        }
    }

    // Check whether each of the successors are unioned. So if you
    // have `Box<X1>` and `Box<X2>`, this is true if `X1 == X2`. (The
    // result of this fn is not really meaningful unless the two nodes
    // are shallow equal here.)
    fn congruent(&mut self, p_u: Token, p_v: Token) -> bool {
        let ss_u: Vec<_> = self.graph.successor_nodes(p_u.node()).collect();
        let ss_v: Vec<_> = self.graph.successor_nodes(p_v.node()).collect();
        ss_u.len() == ss_v.len() &&
        {
            ss_u.into_iter()
                .zip(ss_v.into_iter())
                .all(|(s_u, s_v)| self.unioned(Token::from_node(s_u), Token::from_node(s_v)))
        }
    }

    // Compare the local data, not considering successor nodes. So e.g
    // `Box<X>` and `Box<Y>` are shallow equal for any `X` and `Y`.
    fn shallow_eq(&self, u: Token, v: Token) -> bool {
        let key_u = self.graph.node_data(u.node());
        let key_v = self.graph.node_data(v.node());
        key_u.shallow_eq(key_v)
    }

    fn unioned(&mut self, u: Token, v: Token) -> bool {
        self.table.unioned(u, v)
    }

    fn union(&mut self, u: Token, v: Token) {
        self.table.union(u, v)
    }
}