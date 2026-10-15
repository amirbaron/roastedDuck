//! Row-level access to nested vectors shared by the LIST/STRUCT/MAP/ARRAY functions.

use rustdb_common::{Error, Result};
use rustdb_types::{ListEntry, LogicalType, Sel, SelectionVector, Unified, Vector, VectorKind};

use crate::scalar::executor::addressable;

/// The rows of field `index` of the STRUCT vector `input` (first `count` rows), with the
/// struct's NULLs applied (DuckDB's `StructVector::GetEntries` on a flattened vector).
pub(crate) fn struct_field(input: &Vector, index: usize, count: usize) -> Result<Vector> {
    let input = addressable(input, count);
    let children = input
        .struct_children()
        .ok_or_else(|| Error::internal("struct function called on a non-struct vector"))?;
    let child = children
        .get(index)
        .ok_or_else(|| Error::internal("struct field out of range"))?;
    let mut field = match input.sel() {
        Sel::Identity => child.clone(),
        Sel::Constant => {
            let mut constant = child.slice(&SelectionVector::new(vec![0]));
            constant.flatten(1);
            constant.make_constant();
            constant
        }
        Sel::Indices(indices) => child.slice(&SelectionVector::new(indices.to_vec())),
    };
    // A NULL struct has NULL fields.
    if !input.validity().is_all_valid() {
        let rows = if input.is_constant() { 1 } else { count };
        let nulls: Vec<usize> = (0..rows).filter(|&row| input.is_null(row)).collect();
        if !nulls.is_empty() {
            field.flatten(rows);
            let validity = field.validity_mut();
            for row in nulls {
                validity.set_invalid(row, rows);
            }
            if input.is_constant() {
                field.make_constant();
            }
        }
    }
    Ok(field)
}

/// A LIST vector prepared for row access: its entries and its child vector.
pub(crate) struct ListInput<'a> {
    pub(crate) entries: Unified<'a, ListEntry>,
    pub(crate) child: &'a Vector,
    pub(crate) child_size: usize,
}

impl<'a> ListInput<'a> {
    /// `vector` must be an addressable (non-sequence) LIST or MAP vector.
    pub(crate) fn new(vector: &'a Vector) -> Result<Self> {
        let (child, child_size) = vector
            .list_child()
            .ok_or_else(|| Error::internal("list function called on a non-list vector"))?;
        Ok(Self {
            entries: vector.unified::<ListEntry>(),
            child,
            child_size,
        })
    }

    /// The entry of row `row`, or `None` if the list is NULL.
    #[inline]
    pub(crate) fn get(&self, row: usize) -> Option<ListEntry> {
        self.entries.get(row)
    }
}

/// The child type of a LIST (or ARRAY) type.
pub(crate) fn list_child_type(ty: &LogicalType) -> Result<LogicalType> {
    ty.child_type()
        .or_else(|| ty.array_child().map(|(child, _)| child))
        .cloned()
        .ok_or_else(|| Error::internal(format!("expected a LIST type, got {ty}")))
}

/// Whether every input is a constant vector (the result is then constant too).
pub(crate) fn all_constant(columns: &[Vector]) -> bool {
    columns.iter().all(|column| column.kind() == VectorKind::Constant)
}
