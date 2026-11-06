//! A column of a table or table function, possibly a path into a nested column (DuckDB's
//! `ColumnIndex` of `common/column_index.hpp`).

use crate::{LogicalType, LogicalTypeId};

/// How a [`ColumnIndex`] is read: in its entirety (`FullRead`; its children only say
/// which fields are needed), or as a struct or variant field the scan must extract and
/// emit directly (`PushdownExtract`).
pub use crate::generated::enums::ColumnIndexType;

/// A column, or a field path into a STRUCT (by field index) or VARIANT (by field name)
/// column (DuckDB's `ColumnIndex`). The stored metadata owns native field-name bytes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ColumnIndex {
    /// The column or field index, when `field` is `None` (DuckDB's `has_index`).
    pub index: u64,
    /// The field name of a VARIANT path element.
    pub field: Option<rustdb_common::ByteString>,
    /// The type of the column this references (a pushdown extract), `INVALID` if unset.
    pub ty: LogicalType,
    pub index_type: ColumnIndexType,
    pub child_indexes: Vec<ColumnIndex>,
}

impl Default for ColumnIndex {
    /// DuckDB's default `ColumnIndex()`: the row id column (`INVALID_INDEX`).
    fn default() -> Self {
        Self::new(u64::MAX)
    }
}

impl ColumnIndex {
    pub const fn new(index: u64) -> Self {
        Self {
            index,
            field: None,
            ty: LogicalType::INVALID,
            index_type: ColumnIndexType::FullRead,
            child_indexes: Vec::new(),
        }
    }

    /// A field of a VARIANT, by name.
    pub fn field(field: impl Into<rustdb_common::ByteString>) -> Self {
        Self {
            index: 0,
            field: Some(field.into()),
            ty: LogicalType::INVALID,
            index_type: ColumnIndexType::FullRead,
            child_indexes: Vec::new(),
        }
    }

    /// Column `index` with the given children.
    pub fn with_children(index: u64, child_indexes: Vec<Self>) -> Self {
        Self {
            child_indexes,
            ..Self::new(index)
        }
    }

    /// The same path element (index or field name) without type or children.
    #[must_use]
    pub fn same_element(&self) -> Self {
        match &self.field {
            Some(field) => Self::field(field.clone()),
            None => Self::new(self.index),
        }
    }

    pub const fn has_primary_index(&self) -> bool {
        self.field.is_none()
    }

    pub fn field_name(&self) -> &[u8] {
        self.field.as_deref().unwrap_or_default()
    }

    pub const fn is_row_id_column(&self) -> bool {
        self.field.is_none() && self.index == u64::MAX
    }

    pub fn has_children(&self) -> bool {
        !self.child_indexes.is_empty()
    }

    pub fn has_type(&self) -> bool {
        self.ty.id() != LogicalTypeId::Invalid
    }

    pub fn is_pushdown_extract(&self) -> bool {
        self.index_type == ColumnIndexType::PushdownExtract
    }

    /// Whether this and `other` name the same path element (index or field name).
    pub fn same_element_as(&self, other: &Self) -> bool {
        match (&self.field, &other.field) {
            (None, None) => self.index == other.index,
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// The type a scan of this index produces (`GetScanType`): the leaf's type of a
    /// pushdown extract.
    pub fn scan_type(&self) -> &LogicalType {
        if self.is_pushdown_extract()
            && let Some(child) = self.child_indexes.first()
        {
            return child.scan_type();
        }
        &self.ty
    }

    /// Makes this index a pushdown extract of its single child path out of a column of
    /// `type_information`; the leaf gets `cast_type`, or its field type
    /// (`SetPushdownExtractType`).
    pub fn set_pushdown_extract_type(
        &mut self,
        type_information: &LogicalType,
        cast_type: Option<&LogicalType>,
    ) {
        self.index_type = ColumnIndexType::PushdownExtract;
        self.ty = type_information.clone();
        let Some(child) = self.child_indexes.first_mut() else {
            return;
        };
        if child.has_primary_index() {
            let child_type = type_information
                .struct_children()
                .and_then(|children| children.get(child.index as usize))
                .map_or(LogicalType::INVALID, |(_, ty)| ty.clone());
            if child.child_indexes.is_empty() {
                child.ty = cast_type.cloned().unwrap_or(child_type);
            } else {
                child.set_pushdown_extract_type(&child_type, cast_type);
            }
        } else if child.child_indexes.is_empty() {
            // Without a cast, the child of a VARIANT is a VARIANT.
            child.ty = cast_type.cloned().unwrap_or_else(|| type_information.clone());
        } else {
            child.set_pushdown_extract_type(type_information, cast_type);
        }
    }

    /// Whether this path, which has 0 or 1 children at every level, continues `path`
    /// (`IsChildPathOf`): `path` is a strict prefix of it.
    pub fn is_child_path_of(&self, path: &Self) -> bool {
        let mut a = self;
        let mut b = path;
        loop {
            if !a.same_element_as(b) {
                return false;
            }
            match (a.child_indexes.first(), b.child_indexes.first()) {
                (None, _) => return false,
                (Some(_), None) => return true,
                (Some(next_a), Some(next_b)) => {
                    a = next_a;
                    b = next_b;
                }
            }
        }
    }
}

impl rustdb_serde::Serialize for ColumnIndex {
    /// `ColumnIndex::Serialize`.
    fn serialize(&self, serializer: &mut dyn rustdb_serde::Serializer) -> rustdb_serde::Result<()> {
        let empty = rustdb_common::ByteString::new();
        serializer.write_object(&crate::generated::column_index::ColumnIndex {
            index: self.index,
            child_indexes: &self.child_indexes,
            index_type: self.index_type,
            ty: &self.ty,
            field: self.field.as_ref().unwrap_or(&empty),
            has_index: self.has_primary_index(),
        })
    }
}

impl rustdb_serde::DeserializeObject for ColumnIndex {
    fn deserialize_fields(d: &mut dyn rustdb_serde::Deserializer) -> rustdb_serde::Result<Self> {
        let index = d.read_property_with_default(1, "index")?;
        let child_indexes = d.read_property_with_default(2, "child_indexes")?;
        let index_type = d.read_property_with_explicit_default(3, "index_type", ColumnIndexType::FullRead)?;
        let ty = d.read_property_with_explicit_default(4, "type", LogicalType::INVALID)?;
        let field: rustdb_common::ByteString = d.read_property_with_default(5, "field")?;
        let has_index = d.read_property_with_explicit_default(6, "has_index", true)?;
        Ok(Self {
            index,
            child_indexes,
            index_type,
            ty,
            field: (!has_index).then_some(field),
        })
    }
}
impl rustdb_serde::Deserialize for ColumnIndex {
    fn deserialize(d: &mut dyn rustdb_serde::Deserializer) -> rustdb_serde::Result<Self> {
        d.read_object()
    }
}
