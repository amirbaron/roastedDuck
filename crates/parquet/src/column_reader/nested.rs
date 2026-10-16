//! Readers of nested columns (DuckDB's `ListColumnReader` and `StructColumnReader`).

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{ListEntry, LogicalType, LogicalTypeId, Vector, VectorKind};

use super::{ColumnReader, ReaderContext};
use crate::schema::ColumnSchema;
use crate::thrift::RowGroup;
use crate::vector_ops::{copy_rows, list_append};

/// `StructColumnReader`: one reader per projected field.
#[derive(Debug)]
pub struct StructReader {
    logical_type: LogicalType,
    max_define: u8,
    /// `None` for fields that are not read (they come out NULL).
    pub children: Vec<Option<ColumnReader>>,
}

impl StructReader {
    pub fn new(schema: &ColumnSchema, children: Vec<Option<ColumnReader>>) -> Self {
        Self {
            logical_type: schema.logical_type.clone(),
            max_define: schema.max_define,
            children,
        }
    }

    pub fn logical_type(&self) -> &LogicalType {
        &self.logical_type
    }

    /// The reader of field `index` (`GetChildReader`).
    pub fn child_mut(&mut self, index: usize) -> Result<&mut ColumnReader> {
        self.children
            .get_mut(index)
            .and_then(Option::as_mut)
            .ok_or_else(|| {
                Error::internal(format!(
                    "StructColumnReader::GetChildReader({index}) - but this child reader is not set"
                ))
            })
    }

    pub fn initialize_read(&mut self, row_group_index: usize, row_group: &RowGroup, context: &ReaderContext) -> Result<()> {
        for child in self.children.iter_mut().flatten() {
            child.initialize_read(row_group_index, row_group, context)?;
        }
        Ok(())
    }

    /// `StructColumnReader::Read`.
    pub fn read(&mut self, num_values: usize, defines: &mut [u8], repeats: &mut [u8], result: &mut Vector) -> Result<usize> {
        defines[..num_values].fill(self.max_define);
        if result.kind() != VectorKind::Flat || result.row_capacity() < num_values {
            *result = Vector::flat(self.logical_type.clone(), num_values.max(STANDARD_VECTOR_SIZE));
        }
        let mut read_count = None;
        let entries = result
            .struct_children_mut()
            .ok_or_else(|| Error::internal("STRUCT vector without children"))?;
        for (child, target) in self.children.iter_mut().zip(entries.iter_mut()) {
            let Some(child) = child else {
                *target = Vector::constant_null(target.logical_type().clone());
                continue;
            };
            let child_count = child.read(num_values, defines, repeats, target)?;
            if target.kind() != VectorKind::Flat {
                target.flatten(child_count);
            }
            match read_count {
                None => read_count = Some(child_count),
                Some(count) if count != child_count => {
                    return Err(crate::decode::runtime_error("Struct child row count mismatch"));
                }
                Some(_) => {}
            }
        }
        let read_count = read_count.unwrap_or(num_values);
        for row in 0..read_count {
            if defines[row] < self.max_define {
                result.set_null(row);
            }
        }
        Ok(read_count)
    }

    pub fn skip(&mut self, num_values: usize) {
        for child in self.children.iter_mut().flatten() {
            child.skip(num_values);
        }
    }

    /// `StructColumnReader::GroupRowsAvailable`.
    pub fn group_rows_available(&self) -> Result<u64> {
        for child in self.children.iter().flatten() {
            if type_has_exact_row_count(&child.logical_type()) {
                return child.group_rows_available();
            }
        }
        for child in self.children.iter().flatten() {
            return child.group_rows_available();
        }
        Err(Error::internal("No projected columns in struct?"))
    }

    pub fn total_compressed_size(&self) -> u64 {
        self.children.iter().flatten().map(ColumnReader::total_compressed_size).sum()
    }
}

/// `TypeHasExactRowCount`.
fn type_has_exact_row_count(ty: &LogicalType) -> bool {
    match ty.id() {
        LogicalTypeId::List | LogicalTypeId::Map => false,
        LogicalTypeId::Struct => ty
            .struct_children()
            .is_some_and(|children| children.iter().any(|(_, child)| type_has_exact_row_count(child))),
        _ => true,
    }
}

/// `ListColumnReader`: assembles lists from the repetition levels of its child.
#[derive(Debug)]
pub struct ListReader {
    logical_type: LogicalType,
    child_type: LogicalType,
    max_define: u8,
    max_repeat: u8,
    child: ColumnReader,
    child_defines: Vec<u8>,
    child_repeats: Vec<u8>,
    /// Child values read but not yet consumed by a list.
    read_vector: Vector,
    overflow_child_count: usize,
    pending_skips: usize,
}

impl ListReader {
    pub fn new(schema: &ColumnSchema, child: ColumnReader) -> Self {
        let child_type = schema
            .logical_type
            .child_type()
            .cloned()
            .unwrap_or(LogicalType::SQLNULL);
        Self {
            logical_type: schema.logical_type.clone(),
            read_vector: Vector::flat(child_type.clone(), STANDARD_VECTOR_SIZE),
            child_type,
            max_define: schema.max_define,
            max_repeat: schema.max_repeat,
            child,
            child_defines: vec![0; STANDARD_VECTOR_SIZE],
            child_repeats: vec![0; STANDARD_VECTOR_SIZE],
            overflow_child_count: 0,
            pending_skips: 0,
        }
    }

    pub fn logical_type(&self) -> &LogicalType {
        &self.logical_type
    }

    pub fn initialize_read(&mut self, row_group_index: usize, row_group: &RowGroup, context: &ReaderContext) -> Result<()> {
        self.child.initialize_read(row_group_index, row_group, context)
    }

    pub fn group_rows_available(&self) -> Result<u64> {
        Ok(self.child.group_rows_available()? + self.overflow_child_count as u64)
    }

    pub fn total_compressed_size(&self) -> u64 {
        self.child.total_compressed_size()
    }

    pub fn skip(&mut self, num_values: usize) {
        self.pending_skips += num_values;
    }

    /// `ListColumnReader::Read`.
    pub fn read(&mut self, num_values: usize, defines: &mut [u8], repeats: &mut [u8], result: &mut Vector) -> Result<usize> {
        if self.pending_skips > 0 {
            let skips = std::mem::take(&mut self.pending_skips);
            self.read_internal(skips, None, None, None)?;
        }
        if result.kind() != VectorKind::Flat || result.capacity() < num_values {
            *result = Vector::flat(self.logical_type.clone(), num_values.max(STANDARD_VECTOR_SIZE));
        } else {
            result.set_list_size(0);
        }
        self.read_internal(num_values, Some(defines), Some(repeats), Some(result))
    }

    /// `ListColumnReader::ReadInternal`; without a result vector the lists are skipped.
    fn read_internal(
        &mut self,
        num_values: usize,
        mut defines: Option<&mut [u8]>,
        mut repeats: Option<&mut [u8]>,
        mut result: Option<&mut Vector>,
    ) -> Result<usize> {
        let mut result_offset = 0;
        let mut entries = vec![ListEntry::default(); if result.is_some() { num_values } else { 0 }];
        let mut nulls = Vec::new();
        let mut finished = false;
        while !finished {
            let child_count = if self.overflow_child_count == 0 {
                self.child_defines.fill(0);
                self.child_repeats.fill(0);
                let request = (STANDARD_VECTOR_SIZE as u64).min(self.child.group_rows_available()?) as usize;
                let mut vector = Vector::flat(self.child_type.clone(), STANDARD_VECTOR_SIZE);
                let count = self
                    .child
                    .read(request, &mut self.child_defines, &mut self.child_repeats, &mut vector)?;
                self.read_vector = vector;
                count
            } else {
                std::mem::take(&mut self.overflow_child_count)
            };
            if child_count == 0 {
                break;
            }
            let chunk_offset = result.as_ref().map_or(0, |result| {
                result.list_child().map_or(0, |(_, size)| size)
            });
            let mut child_index = 0;
            while child_index < child_count {
                if self.child_repeats[child_index] == self.max_repeat {
                    if result_offset == 0 {
                        return Err(crate::decode::runtime_error(
                            "Parquet file is likely corrupted, repetition level without a list",
                        ));
                    }
                    if let Some(entry) = entries.get_mut(result_offset - 1) {
                        entry.length += 1;
                    }
                    child_index += 1;
                    continue;
                }
                if result_offset >= num_values {
                    finished = true;
                    break;
                }
                let level = self.child_defines[child_index];
                if let Some(entry) = entries.get_mut(result_offset) {
                    if level >= self.max_define {
                        *entry = ListEntry::new(child_index + chunk_offset, 1);
                    } else if level == self.max_define - 1 {
                        *entry = ListEntry::new(child_index + chunk_offset, 0);
                    } else {
                        *entry = ListEntry::default();
                        nulls.push(result_offset);
                    }
                }
                if let Some(repeats) = repeats.as_deref_mut() {
                    repeats[result_offset] = self.child_repeats[child_index];
                }
                if let Some(defines) = defines.as_deref_mut() {
                    defines[result_offset] = self.child_defines[child_index];
                }
                result_offset += 1;
                child_index += 1;
            }
            if let Some(result) = result.as_deref_mut() {
                list_append(result, &self.read_vector, child_index)?;
            }
            if child_index < child_count && result_offset == num_values {
                let remaining = child_count - child_index;
                let mut rest = Vector::flat(self.child_type.clone(), STANDARD_VECTOR_SIZE);
                copy_rows(&self.read_vector, child_index, remaining, &mut rest, 0)?;
                self.read_vector = rest;
                self.overflow_child_count = remaining;
                self.child_defines.copy_within(child_index..child_count, 0);
                self.child_repeats.copy_within(child_index..child_count, 0);
            }
        }
        if let Some(result) = result {
            result.values_mut::<ListEntry>()[..entries.len()].copy_from_slice(&entries);
            let capacity = result.capacity();
            for row in nulls {
                result.validity_mut().set_invalid(row, capacity);
            }
        }
        Ok(result_offset)
    }
}
