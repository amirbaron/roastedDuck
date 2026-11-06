//! Sets of join relations (DuckDB's `JoinRelationSet` and `JoinRelationSetManager`).
//!
//! DuckDB interns every set in a trie so that sets compare by address. Here a set is a
//! sorted list of relation ids compared by value, which gives the same identities.

use std::fmt;

/// A sorted, duplicate-free set of relation ids (DuckDB's `JoinRelationSet`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelationSet(Vec<usize>);

impl RelationSet {
    /// The set of the given relations (DuckDB's `GetJoinRelation(unordered_set)`).
    pub fn new(relations: impl IntoIterator<Item = usize>) -> Self {
        let mut relations: Vec<usize> = relations.into_iter().collect();
        relations.sort_unstable();
        relations.dedup();
        Self(relations)
    }

    /// The set of one relation (DuckDB's `GetJoinRelation(idx_t)`).
    pub fn single(relation: usize) -> Self {
        Self(vec![relation])
    }

    /// The relations, in increasing order.
    pub fn relations(&self) -> &[usize] {
        &self.0
    }

    /// The number of relations.
    pub fn count(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The union of two sets (DuckDB's `JoinRelationSetManager::Union`).
    pub fn union(&self, other: &Self) -> Self {
        let mut relations = Vec::with_capacity(self.0.len() + other.0.len());
        let (mut i, mut j) = (0, 0);
        while i < self.0.len() || j < other.0.len() {
            match (self.0.get(i), other.0.get(j)) {
                (Some(&left), Some(&right)) if left < right => {
                    relations.push(left);
                    i += 1;
                }
                (Some(&left), Some(&right)) if left > right => {
                    relations.push(right);
                    j += 1;
                }
                (Some(&left), Some(_)) => {
                    relations.push(left);
                    i += 1;
                    j += 1;
                }
                (Some(&left), None) => {
                    relations.push(left);
                    i += 1;
                }
                (None, Some(&right)) => {
                    relations.push(right);
                    j += 1;
                }
                (None, None) => break,
            }
        }
        Self(relations)
    }

    /// Whether `sub` is contained in `self` (DuckDB's `JoinRelationSet::IsSubset(super, sub)`).
    pub fn contains_set(&self, sub: &Self) -> bool {
        if sub.0.is_empty() || sub.0.len() > self.0.len() {
            return false;
        }
        let mut j = 0;
        for &relation in &self.0 {
            if sub.0[j] == relation {
                j += 1;
                if j == sub.0.len() {
                    return true;
                }
            }
        }
        false
    }
}

impl fmt::Display for RelationSet {
    /// DuckDB's `JoinRelationSet::ToString`, e.g. `[0, 2]`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[")?;
        for (i, relation) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{relation}")?;
        }
        f.write_str("]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_and_subsets() {
        let left = RelationSet::new([3, 1]);
        let right = RelationSet::new([2, 3]);
        let union = left.union(&right);
        assert_eq!(union.relations(), &[1, 2, 3]);
        assert!(union.contains_set(&left));
        assert!(!left.contains_set(&union));
        assert_eq!(union.to_string(), "[1, 2, 3]");
    }
}
