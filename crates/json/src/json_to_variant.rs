//! JSON to VARIANT (DuckDB's `json_to_variant.hpp`, `ConvertJSONToVariant`): values are
//! written depth first; objects become OBJECTs (keys in order, duplicates kept), arrays
//! ARRAYs, strings VARCHAR, unsigned integers UINT64, signed ones INT64 and other numbers
//! (bignums included) DOUBLE. A JSON `null` at the top is a NULL VARIANT.

use rustdb_common::{Error, Result};
use rustdb_function::cast::text_parse::TextFloat;
use rustdb_function::cast::{BoundCastInfo, CastParameters};
use rustdb_types::Vector;
use rustdb_types::variant::{
    VariantChild, VariantData, VariantEntry, VariantLogicalType, varint_encode, varint_size,
};

use crate::json_common::read_document;
use crate::json_executors::{StrInput, addressable};
use crate::yyjson::{Doc, JsonArena, ValId, ValType, tag};

/// `u32` of a list position.
fn index(position: usize) -> Result<u32> {
    u32::try_from(position).map_err(|_| Error::internal("VARIANT too large"))
}

/// Builds one VARIANT value (the lists of `ToVariantGlobalResultData` for one row).
#[derive(Debug, Default)]
struct VariantBuilder {
    data: VariantData,
}

impl VariantBuilder {
    /// `WriteVariantMetadata`: the value entry, with its payload at the end of the data.
    fn metadata(&mut self, type_id: VariantLogicalType) -> Result<()> {
        let byte_offset = index(self.data.data.len())?;
        self.data.values.push(VariantEntry {
            type_id: type_id as u8,
            byte_offset,
        });
        Ok(())
    }

    /// `WriteContainerData` and the reservation of the container's children (and keys).
    fn container(&mut self, count: usize, keys: Option<Vec<rustdb_common::ByteString>>) -> Result<usize> {
        let children_offset = self.data.children.len();
        varint_encode(count as u64, &mut self.data.data);
        if count > 0 {
            varint_encode(children_offset as u64, &mut self.data.data);
        } else {
            // WriteContainerData reserves the children-offset width even when
            // an empty container does not write that offset. Keep it initialized.
            self.data
                .data
                .resize(self.data.data.len() + varint_size(children_offset as u64), 0);
        }
        let keys_offset = self.data.keys.len();
        let has_keys = keys.is_some();
        if let Some(keys) = keys {
            self.data.keys.extend(keys);
        }
        for i in 0..count {
            self.data.children.push(VariantChild {
                keys_index: if has_keys {
                    Some(index(keys_offset + i)?)
                } else {
                    None
                },
                values_index: 0,
            });
        }
        Ok(children_offset)
    }

    /// `ConvertJSON`: appends value `val`; `false` if it cannot be converted.
    fn convert(&mut self, doc: &Doc<'_>, val: ValId) -> Result<bool> {
        match doc.get_type(val) {
            ValType::Null | ValType::None => self.metadata(VariantLogicalType::VariantNull)?,
            ValType::Arr => {
                self.metadata(VariantLogicalType::Array)?;
                let start = self.container(doc.len(val), None)?;
                for (i, element) in doc.arr_iter(val).enumerate() {
                    self.data.children[start + i].values_index = index(self.data.values.len())?;
                    if !self.convert(doc, element)? {
                        return Ok(false);
                    }
                }
            }
            ValType::Obj => {
                self.metadata(VariantLogicalType::Object)?;
                let keys = doc
                    .obj_iter(val)
                    .map(|(key, _)| rustdb_common::ByteString::from(doc.str(key)))
                    .collect();
                let start = self.container(doc.len(val), Some(keys))?;
                for (i, (_, child)) in doc.obj_iter(val).enumerate() {
                    self.data.children[start + i].values_index = index(self.data.values.len())?;
                    if !self.convert(doc, child)? {
                        return Ok(false);
                    }
                }
            }
            ValType::Str => {
                self.metadata(VariantLogicalType::Varchar)?;
                let bytes = doc.str(val);
                varint_encode(bytes.len() as u64, &mut self.data.data);
                self.data.data.extend_from_slice(bytes);
            }
            ValType::Bool => self.metadata(if doc.get_bool(val) {
                VariantLogicalType::BoolTrue
            } else {
                VariantLogicalType::BoolFalse
            })?,
            ValType::Num => match doc.tag(val) {
                tag::UINT => {
                    self.metadata(VariantLogicalType::Uint64)?;
                    self.data.data.extend_from_slice(&doc.get_uint(val).to_le_bytes());
                }
                tag::SINT => {
                    self.metadata(VariantLogicalType::Int64)?;
                    self.data.data.extend_from_slice(&doc.get_sint(val).to_le_bytes());
                }
                _ => {
                    self.metadata(VariantLogicalType::Double)?;
                    self.data.data.extend_from_slice(&doc.get_real(val).to_le_bytes());
                }
            },
            ValType::Raw => {
                let Some(value) = f64::parse_float_with(doc.str(val), b'.', true) else {
                    return Ok(false);
                };
                self.metadata(VariantLogicalType::Double)?;
                self.data.data.extend_from_slice(&value.to_le_bytes());
            }
        }
        Ok(true)
    }
}

/// The cast from JSON to VARIANT.
pub(crate) fn json_to_variant_cast() -> BoundCastInfo {
    BoundCastInfo::new(
        |source: &Vector, result: &mut Vector, count: usize, params: &mut CastParameters| {
            let rows = if source.is_constant() { count.min(1) } else { count };
            let column = addressable(source, rows);
            let input = StrInput::new(&column);
            let target = result.logical_type().clone();
            let mut out = Vector::flat(target, rows.max(1));
            let mut arena = JsonArena::default();
            let mut success = true;
            for row in 0..rows {
                let Some(json) = input.get(row) else {
                    out.set_null(row);
                    continue;
                };
                arena.reset();
                let doc = read_document(&mut arena, json)?;
                let view = arena.doc();
                if view.is_null(doc.root) {
                    out.set_null(row);
                    continue;
                }
                let mut builder = VariantBuilder::default();
                if !builder.convert(&view, doc.root)? {
                    out.set_null(row);
                    if success {
                        params.assign_error("Failed to convert JSON value to VARIANT".to_owned())?;
                        success = false;
                    }
                    continue;
                }
                out.set_value(row, &builder.data.to_value())?;
            }
            if source.is_constant() {
                out.make_constant();
            }
            *result = out;
            Ok(success)
        },
    )
}
