//! Hand-written query nodes: `SetOperationNode`, whose serialized form depends on the
//! target version, and the legacy `CTENode`.

use rustdb_serde::{DeserializeObject, Deserializer, Error, Result, SerializeObject, Serializer, required};

use crate::{CommonTableExpressionMap, CteMaterialize, QueryNode, QueryNodeKind, SetOperationType};

/// The serialization version that writes all children of a set operation in one list.
const CHILD_LIST_VERSION: u64 = 7;

/// `UNION`, `EXCEPT`, `INTERSECT` (and `UNION BY NAME`) over two or more queries
/// (DuckDB's `SetOperationNode`).
#[derive(Clone, Debug, PartialEq)]
pub struct SetOperationNode {
    pub setop_type: SetOperationType,
    /// `ALL` (keep duplicates).
    pub setop_all: bool,
    /// The operands, at least two.
    pub children: Vec<QueryNode>,
}

impl SetOperationNode {
    /// The operands after the first as one node, for readers that only know binary set
    /// operations (DuckDB's `SerializeChildNode(serializer, 1)`): a balanced tree of set
    /// operations of this kind.
    fn folded_right_operand(&self) -> Option<QueryNode> {
        let mut nodes: Vec<QueryNode> = self.children.iter().skip(1).cloned().collect();
        while nodes.len() > 1 {
            let mut merged = Vec::with_capacity(nodes.len().div_ceil(2));
            let mut pending = nodes.into_iter();
            while let Some(left) = pending.next() {
                match pending.next() {
                    Some(right) => merged.push(QueryNode {
                        modifiers: Vec::new(),
                        cte_map: CommonTableExpressionMap::default(),
                        kind: QueryNodeKind::SetOperation(Self {
                            setop_type: self.setop_type,
                            setop_all: self.setop_all,
                            children: vec![left, right],
                        }),
                    }),
                    None => merged.push(left),
                }
            }
            nodes = merged;
        }
        nodes.pop()
    }
}

impl SerializeObject for SetOperationNode {
    fn serialize_fields(&self, serializer: &mut dyn Serializer) -> Result<()> {
        serializer.write_property(200, "setop_type", &self.setop_type)?;
        let child_list = serializer.should_serialize(CHILD_LIST_VERSION);
        if child_list {
            let none: Option<Box<QueryNode>> = None;
            serializer.write_property_with_default(201, "left", &none)?;
            serializer.write_property_with_default(202, "right", &none)?;
        } else {
            let (Some(left), Some(second)) = (self.children.first(), self.children.get(1)) else {
                return Err(Error::serialization(
                    "SetOperationNode must have at least two children",
                ));
            };
            serializer.write_optional(201, "left", false, left)?;
            if self.children.len() == 2 {
                serializer.write_optional(202, "right", false, second)?;
            } else {
                let right = self.folded_right_operand();
                serializer.write_property_with_default(202, "right", &right)?;
            }
        }
        serializer.write_property_with_explicit_default(203, "setop_all", &self.setop_all, &true)?;
        if child_list {
            serializer.write_property_with_default(204, "children", &self.children)?;
        }
        Ok(())
    }
}

impl DeserializeObject for SetOperationNode {
    fn deserialize_fields(deserializer: &mut dyn Deserializer) -> Result<Self> {
        let setop_type = deserializer.read_property(200, "setop_type")?;
        let left: Option<Box<QueryNode>> = deserializer.read_property_with_default(201, "left")?;
        let right: Option<Box<QueryNode>> = deserializer.read_property_with_default(202, "right")?;
        let setop_all = deserializer.read_property_with_explicit_default(203, "setop_all", true)?;
        let children: Vec<QueryNode> = deserializer.read_property_with_default(204, "children")?;
        let children = match (left, right, children.is_empty()) {
            (Some(left), Some(right), true) => vec![*left, *right],
            (None, None, false) => children,
            _ => {
                return Err(Error::serialization(
                    "Error deserializing SetOperationNode - left/right or children must be set",
                ));
            }
        };
        if children.len() < 2 {
            return Err(Error::serialization(
                "SetOperationNode must have at least two children",
            ));
        }
        Ok(Self {
            setop_type,
            setop_all,
            children,
        })
    }
}

/// Reads the members of a legacy `CTE_NODE` and returns the query it wraps: CTEs now
/// live in the query's `cte_map`, so DuckDB discards the wrapper on load.
pub(crate) fn read_legacy_cte_node(deserializer: &mut dyn Deserializer) -> Result<QueryNode> {
    deserializer.read_property_with_default::<String>(200, "cte_name")?;
    deserializer.read_property_with_default::<Option<Box<QueryNode>>>(201, "query")?;
    let child = deserializer.read_property_with_default(202, "child")?;
    deserializer.read_property_with_default::<Vec<String>>(203, "aliases")?;
    deserializer.read_property_with_explicit_default(
        204,
        "materialized",
        CteMaterialize::CteMaterializeDefault,
    )?;
    required::<Box<QueryNode>>(child, "CTENode", "child").map(|child| *child)
}
