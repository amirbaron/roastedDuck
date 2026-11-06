//! Builds Arrow arrays from DuckDB vectors (DuckDB's `common/arrow/arrow_appender.cpp`,
//! `arrow_buffer.hpp` and the `appender/*` append data of each type).
//!
//! An [`ArrowAppender`] collects chunks column by column into [`ArrowAppendData`]: the
//! Arrow buffers of a column and of its children. Finalizing hands each column's
//! append data to the Arrow array as its private data, so the buffers live until the
//! consumer releases the array.

use std::alloc::Layout;
use std::borrow::Cow;
use std::ffi::c_void;
use std::sync::Arc;

use rustdb_common::{Error, Result, STANDARD_VECTOR_SIZE};
use rustdb_types::{
    Interval, ListEntry, LogicalType, LogicalTypeId, NativeType, PhysicalType, Sel, SelectionVector, StringT,
    TimeTz, Uuid, ValidityMask, Vector, VectorKind,
};

use crate::client_properties::{ArrowFormatVersion, ArrowOffsetSize, ClientProperties};
use crate::ffi::ArrowArray;
use crate::type_extension::{ArrowTypeExtensionData, arrow_type_extensions};

/// The alignment of buffer memory: that of `malloc`, which DuckDB's buffers come from.
const BUFFER_ALIGNMENT: usize = 16;

/// A growable byte buffer handed to Arrow consumers (DuckDB's `ArrowBuffer`). Capacity
/// grows in powers of two; new bytes are zeroed.
#[derive(Debug)]
pub(crate) struct ArrowBuffer {
    /// NULL until the first reservation.
    data: *mut u8,
    count: usize,
    capacity: usize,
}

impl Default for ArrowBuffer {
    fn default() -> Self {
        Self {
            data: std::ptr::null_mut(),
            count: 0,
            capacity: 0,
        }
    }
}

impl Drop for ArrowBuffer {
    fn drop(&mut self) {
        if !self.data.is_null() {
            // SAFETY: `data` was allocated with this layout by `reserve`.
            unsafe { std::alloc::dealloc(self.data, Self::layout(self.capacity)) };
        }
    }
}

impl ArrowBuffer {
    fn layout(capacity: usize) -> Layout {
        Layout::from_size_align(capacity, BUFFER_ALIGNMENT).unwrap_or_else(|_| {
            // A capacity that overflows a layout cannot be allocated either.
            std::alloc::handle_alloc_error(Layout::new::<u8>())
        })
    }

    /// Ensures room for `bytes` bytes, rounded up to a power of two (`reserve`).
    fn reserve(&mut self, bytes: usize) {
        let new_capacity = bytes.next_power_of_two();
        if new_capacity <= self.capacity {
            return;
        }
        let layout = Self::layout(new_capacity);
        let data = if self.data.is_null() {
            // SAFETY: `layout` has a non-zero size (a power of two).
            unsafe { std::alloc::alloc(layout) }
        } else {
            // SAFETY: `data` was allocated with the layout of `capacity`; the new size is
            // valid for the alignment (checked by `layout`).
            unsafe { std::alloc::realloc(self.data, Self::layout(self.capacity), new_capacity) }
        };
        if data.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        self.data = data;
        self.capacity = new_capacity;
    }

    /// Sets the length to `bytes`, zeroing new bytes (`resize`).
    fn resize(&mut self, bytes: usize) {
        self.resize_with(bytes, 0);
    }

    /// Sets the length to `bytes`, new bytes set to `value` (`resize(bytes, value)`).
    fn resize_with(&mut self, bytes: usize, value: u8) {
        self.reserve(bytes);
        if bytes > self.count {
            // SAFETY: `reserve` made room for `bytes` bytes.
            unsafe { std::ptr::write_bytes(self.data.add(self.count), value, bytes - self.count) };
        }
        self.count = bytes;
    }

    /// Appends a value (`push_back`).
    fn push<T: Copy>(&mut self, value: T) {
        let index = self.count / size_of::<T>();
        self.resize(self.count + size_of::<T>());
        self.write(index, value);
    }

    fn len(&self) -> usize {
        self.count
    }

    /// The buffer for Arrow (`data`): NULL if nothing was ever reserved.
    fn data(&self) -> *const c_void {
        self.data.cast()
    }

    /// The bytes in use.
    fn bytes_mut(&mut self) -> &mut [u8] {
        if self.data.is_null() {
            return &mut [];
        }
        // SAFETY: the first `count` bytes are allocated and initialized.
        unsafe { std::slice::from_raw_parts_mut(self.data, self.count) }
    }

    /// Writes element `index` of the buffer seen as `[T]`.
    fn write<T: Copy>(&mut self, index: usize, value: T) {
        let offset = index * size_of::<T>();
        assert!(
            offset + size_of::<T>() <= self.count,
            "ArrowBuffer write out of bounds"
        );
        // SAFETY: the element lies within the bytes in use (checked above).
        unsafe { self.data.add(offset).cast::<T>().write_unaligned(value) };
    }

    /// Reads element `index` of the buffer seen as `[T]`.
    fn read<T: Copy>(&self, index: usize) -> T {
        let offset = index * size_of::<T>();
        assert!(
            offset + size_of::<T>() <= self.count,
            "ArrowBuffer read out of bounds"
        );
        // SAFETY: the element lies within the initialized bytes in use (checked above).
        unsafe { self.data.add(offset).cast::<T>().read_unaligned() }
    }
}

/// An Arrow `month_day_nano` interval (DuckDB's `ArrowInterval`).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct ArrowInterval {
    months: i32,
    days: i32,
    nanoseconds: i64,
}

/// How fixed-width values are converted (the `OP` of DuckDB's `ArrowScalarData`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScalarOp {
    /// The values as they are (`ArrowScalarConverter`).
    Copy,
    /// 16-bit decimals as 32-bit ones.
    WidenTo32,
    /// Decimals as 128-bit ones.
    WidenTo128,
    /// The time of a `TIME WITH TIME ZONE`, in microseconds (`ArrowTimeTzConverter`).
    TimeTz,
    /// `ArrowIntervalConverter`
    Interval,
    /// A UUID as its 16 bytes (`ArrowUUIDBlobConverter`).
    UuidBlob,
}

/// The layout a column is appended in (the append functions of DuckDB's
/// `InitializeFunctionPointers`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppendKind {
    /// `ArrowNullData`
    Null,
    /// `ArrowBoolData`
    Bool,
    /// `ArrowScalarData`
    Scalar(ScalarOp),
    /// `ArrowVarcharData`: strings, with 64-bit offsets when `large`.
    Varchar { large: bool },
    /// `ArrowVarcharData<hugeint_t, ArrowUUIDConverter>`: UUIDs as text.
    UuidText { large: bool },
    /// `ArrowVarcharToStringViewData`
    StringView,
    /// `ArrowEnumData`: the indices, with the values as a dictionary.
    Enum,
    /// `ArrowStructData`
    Struct,
    /// `ArrowUnionData`: a sparse union.
    Union,
    /// `ArrowFixedSizeListData`
    FixedSizeList,
    /// `ArrowListData`
    List { large: bool },
    /// `ArrowListViewData`
    ListView { large: bool },
    /// `ArrowMapData`
    Map,
    /// The `entries` struct of a map, filled by the map.
    MapEntries,
}

/// The state of appending one column (or child) into Arrow buffers; after finalizing,
/// the private data of its Arrow array (DuckDB's `ArrowAppendData`).
#[derive(Debug)]
pub(crate) struct ArrowAppendData {
    row_count: usize,
    null_count: usize,
    kind: AppendKind,
    /// The extension type the column is exported as, if any.
    extension_data: Option<Arc<ArrowTypeExtensionData>>,
    child_data: Vec<ArrowAppendData>,
    /// The buffer pointers of the finalized array.
    buffers: [*const c_void; 4],
    child_pointers: Vec<*mut ArrowArray>,
    /// The finalized children; the array's `children` point to them.
    child_arrays: Vec<ArrowArray>,
    dictionary: ArrowArray,
    options: ClientProperties,
    /// Where the next long string goes in the data buffer of a string view.
    offset: usize,
    /// Validity, main, auxiliary and (for string views) buffer-size buffers.
    arrow_buffers: [ArrowBuffer; 4],
}

impl Drop for ArrowAppendData {
    fn drop(&mut self) {
        // Children that were moved out of the array were marked released by their consumer.
        for child in self
            .child_arrays
            .iter_mut()
            .chain(std::iter::once(&mut self.dictionary))
        {
            if let Some(release) = child.release {
                // SAFETY: an unreleased child is owned by this array.
                unsafe { release(child) };
            }
        }
    }
}

/// The release callback of arrays the appender made (`ArrowAppender::ReleaseArray`).
unsafe extern "C" fn release_array(array: *mut ArrowArray) {
    // SAFETY: called by the consumer with an array this module made.
    let Some(array) = (unsafe { array.as_mut() }) else {
        return;
    };
    if array.release.is_none() {
        return;
    }
    array.release = None;
    let data = std::mem::replace(&mut array.private_data, std::ptr::null_mut());
    // SAFETY: the private data is the append data `finalize_child` leaked; dropping it
    // releases the children.
    drop(unsafe { Box::from_raw(data.cast::<ArrowAppendData>()) });
}

/// Builds an Arrow struct array from data chunks (DuckDB's `ArrowAppender`).
#[derive(Debug)]
pub struct ArrowAppender {
    types: Vec<LogicalType>,
    root_data: Vec<ArrowAppendData>,
    row_count: usize,
    options: ClientProperties,
}

impl ArrowAppender {
    /// An appender of chunks of `types`, with room for `initial_capacity` rows. Columns of
    /// types with a registered Arrow extension are exported as it, nested ones included
    /// (DuckDB's explicit per-column extensions are those same ones).
    pub fn new(types: Vec<LogicalType>, initial_capacity: usize, options: ClientProperties) -> Result<Self> {
        let root_data = types
            .iter()
            .map(|ty| ArrowAppendData::initialize_child(ty, initial_capacity, &options))
            .collect::<Result<_>>()?;
        Ok(Self {
            types,
            root_data,
            row_count: 0,
            options,
        })
    }

    /// Appends rows `from..to` of `input`, a chunk of `input_size` rows (`Append`).
    pub fn append(
        &mut self,
        input: &rustdb_types::DataChunk,
        from: usize,
        to: usize,
        input_size: usize,
    ) -> Result<()> {
        for (data, column) in self.root_data.iter_mut().zip(input.columns()) {
            data.append_child(column, from, to, input_size)?;
        }
        self.row_count += to - from;
        Ok(())
    }

    /// The struct array of the appended rows; releasing it frees every buffer
    /// (`Finalize`).
    pub fn finalize(self) -> Result<ArrowArray> {
        let mut root = ArrowAppendData::new(AppendKind::Struct, self.options);
        root.child_data = self.root_data;
        root.add_children(self.types.len());
        for (index, (ty, data)) in self
            .types
            .iter()
            .zip(std::mem::take(&mut root.child_data))
            .enumerate()
        {
            root.child_arrays[index] = finalize_child(ty, data)?;
        }
        let root = Box::into_raw(Box::new(root));
        // SAFETY: `root` was just leaked; the array owns it from here on.
        let root_ref = unsafe { &mut *root };
        Ok(ArrowArray {
            length: self.row_count as i64,
            // The root has no NULLs.
            null_count: 0,
            offset: 0,
            n_buffers: 1,
            n_children: self.types.len() as i64,
            buffers: root_ref.buffers.as_mut_ptr(),
            children: root_ref.child_pointers.as_mut_ptr(),
            dictionary: std::ptr::null_mut(),
            release: Some(release_array),
            private_data: root.cast(),
        })
    }
}

/// The finalized array of a child's append data, which it then owns
/// (`ArrowAppender::FinalizeChild`).
fn finalize_child(ty: &LogicalType, data: ArrowAppendData) -> Result<ArrowArray> {
    let data = Box::into_raw(Box::new(data));
    // SAFETY: `data` was just leaked; the array owns it from here on.
    let data_ref = unsafe { &mut *data };
    data_ref.buffers[0] = data_ref.arrow_buffers[0].data();
    let mut result = ArrowArray {
        length: data_ref.row_count as i64,
        null_count: data_ref.null_count as i64,
        offset: 0,
        n_buffers: 0,
        n_children: 0,
        buffers: data_ref.buffers.as_mut_ptr(),
        children: std::ptr::null_mut(),
        dictionary: std::ptr::null_mut(),
        release: Some(release_array),
        private_data: data.cast(),
    };
    if let Err(error) = data_ref.finalize(ty, &mut result) {
        // SAFETY: `result` owns the leaked append data.
        unsafe { release_array(&raw mut result) };
        return Err(error);
    }
    Ok(result)
}

/// The vector of `count` rows with sequences flattened, so its values can be read.
fn readable(input: &Vector, count: usize) -> Cow<'_, Vector> {
    if input.kind() == VectorKind::Sequence {
        Cow::Owned(input.flattened(count))
    } else {
        Cow::Borrowed(input)
    }
}

/// The vector of `count` rows as a flat vector, whose children (of a STRUCT or ARRAY)
/// line up with its rows.
fn flat(input: &Vector, count: usize) -> Cow<'_, Vector> {
    if input.kind() == VectorKind::Flat {
        Cow::Borrowed(input)
    } else {
        Cow::Owned(input.flattened(count))
    }
}

/// Bit `row` of a byte-packed bitmap.
fn bit_position(row: usize) -> (usize, u8) {
    (row / 8, (row % 8) as u8)
}

/// The width of a value of the given layout.
fn scalar_width(op: ScalarOp, physical: PhysicalType) -> usize {
    match op {
        ScalarOp::Copy => physical.size(),
        ScalarOp::WidenTo32 => 4,
        ScalarOp::TimeTz => 8,
        ScalarOp::WidenTo128 | ScalarOp::Interval | ScalarOp::UuidBlob => 16,
    }
}

/// The layout a column of `ty` is appended in (`InitializeFunctionPointers`).
fn append_kind(ty: &LogicalType, options: &ClientProperties) -> Result<AppendKind> {
    let large = options.arrow_offset_size == ArrowOffsetSize::Large;
    let lossless = options.arrow_lossless_conversion;
    let views = options.arrow_output_version >= ArrowFormatVersion::V1_4;
    Ok(match ty.id() {
        LogicalTypeId::SqlNull => AppendKind::Null,
        LogicalTypeId::Boolean => AppendKind::Bool,
        LogicalTypeId::TimeTz if !lossless => AppendKind::Scalar(ScalarOp::TimeTz),
        LogicalTypeId::TinyInt
        | LogicalTypeId::SmallInt
        | LogicalTypeId::Date
        | LogicalTypeId::Integer
        | LogicalTypeId::TimeTz
        | LogicalTypeId::Time
        | LogicalTypeId::TimeNs
        | LogicalTypeId::TimestampSec
        | LogicalTypeId::TimestampMs
        | LogicalTypeId::Timestamp
        | LogicalTypeId::TimestampNs
        | LogicalTypeId::TimestampTz
        | LogicalTypeId::BigInt
        | LogicalTypeId::HugeInt
        | LogicalTypeId::UHugeInt
        | LogicalTypeId::UTinyInt
        | LogicalTypeId::USmallInt
        | LogicalTypeId::UInteger
        | LogicalTypeId::UBigInt
        | LogicalTypeId::Float
        | LogicalTypeId::Double => AppendKind::Scalar(ScalarOp::Copy),
        LogicalTypeId::Uuid if lossless => AppendKind::Scalar(ScalarOp::UuidBlob),
        LogicalTypeId::Uuid => AppendKind::UuidText { large },
        // From version 1.5 decimals keep their width (16-bit ones become 32-bit).
        LogicalTypeId::Decimal => match ty.physical_type() {
            PhysicalType::Int16 if options.arrow_output_version > ArrowFormatVersion::V1_4 => {
                AppendKind::Scalar(ScalarOp::WidenTo32)
            }
            PhysicalType::Int32 | PhysicalType::Int64
                if options.arrow_output_version > ArrowFormatVersion::V1_4 =>
            {
                AppendKind::Scalar(ScalarOp::Copy)
            }
            PhysicalType::Int16 | PhysicalType::Int32 | PhysicalType::Int64 => {
                AppendKind::Scalar(ScalarOp::WidenTo128)
            }
            PhysicalType::Int128 => AppendKind::Scalar(ScalarOp::Copy),
            _ => return Err(Error::internal("Unsupported internal decimal type")),
        },
        LogicalTypeId::Varchar | LogicalTypeId::Blob | LogicalTypeId::Bit | LogicalTypeId::Bignum => {
            if (options.produce_arrow_string_view || ty.id() != LogicalTypeId::Varchar) && views {
                AppendKind::StringView
            } else {
                AppendKind::Varchar { large }
            }
        }
        LogicalTypeId::Enum => match ty.physical_type() {
            PhysicalType::UInt8 | PhysicalType::UInt16 | PhysicalType::UInt32 => AppendKind::Enum,
            _ => return Err(Error::internal("Unsupported internal enum type")),
        },
        LogicalTypeId::Interval => AppendKind::Scalar(ScalarOp::Interval),
        LogicalTypeId::Union => AppendKind::Union,
        LogicalTypeId::Struct => AppendKind::Struct,
        LogicalTypeId::Array => AppendKind::FixedSizeList,
        LogicalTypeId::List if options.arrow_use_list_view && views => AppendKind::ListView { large },
        LogicalTypeId::List => AppendKind::List { large },
        // Arrow maps only have 32-bit offsets.
        LogicalTypeId::Map => AppendKind::Map,
        _ => {
            return Err(Error::not_implemented(format!(
                "Unsupported type in DuckDB -> Arrow Conversion: {ty}\n"
            )));
        }
    })
}

/// The children of a nested type, failing for a type without them.
fn nested_children<'a>(
    ty: &'a LogicalType,
    what: &str,
) -> Result<&'a [(rustdb_common::ByteString, LogicalType)]> {
    match ty.id() {
        LogicalTypeId::Union => ty.union_members(),
        _ => ty.struct_children(),
    }
    .ok_or_else(|| Error::internal(format!("{what} type without children")))
}

/// The element type of a LIST or MAP.
fn list_child_type(ty: &LogicalType) -> Result<&LogicalType> {
    ty.child_type()
        .ok_or_else(|| Error::internal("LIST type without a child type"))
}

impl ArrowAppendData {
    fn new(kind: AppendKind, options: ClientProperties) -> Self {
        Self {
            row_count: 0,
            null_count: 0,
            kind,
            extension_data: None,
            child_data: Vec::new(),
            buffers: [std::ptr::null(); 4],
            child_pointers: Vec::new(),
            child_arrays: Vec::new(),
            dictionary: ArrowArray::empty(),
            options,
            offset: 0,
            arrow_buffers: Default::default(),
        }
    }

    /// The append data of a column of `ty` with room for `capacity` rows: a type with a
    /// registered extension is appended as the extension's internal type, except
    /// BOOLEAN, which is bit-packed unless the conversion is lossless
    /// (`ArrowAppender::InitializeChild`).
    fn initialize_child(ty: &LogicalType, capacity: usize, options: &ClientProperties) -> Result<Self> {
        let bitshift_boolean = ty.id() == LogicalTypeId::Boolean && !options.arrow_lossless_conversion;
        let extension = if bitshift_boolean {
            None
        } else {
            arrow_type_extensions()
                .for_type(ty)
                .map(|extension| Arc::clone(extension.type_extension()))
        };
        let array_type = extension
            .as_ref()
            .map_or(ty, |extension| extension.internal_type());
        let mut result = Self::new(append_kind(array_type, options)?, options.clone());
        result.validity().reserve(capacity.div_ceil(8));
        result.initialize(array_type, capacity)?;
        result.extension_data = extension;
        Ok(result)
    }

    fn validity(&mut self) -> &mut ArrowBuffer {
        &mut self.arrow_buffers[0]
    }

    fn main_buffer(&mut self) -> &mut ArrowBuffer {
        &mut self.arrow_buffers[1]
    }

    fn aux_buffer(&mut self) -> &mut ArrowBuffer {
        &mut self.arrow_buffers[2]
    }

    /// Sizes the children of the finalized array (`ArrowAppender::AddChildren`).
    fn add_children(&mut self, count: usize) {
        self.child_arrays = (0..count).map(|_| ArrowArray::empty()).collect();
        self.child_pointers = self.child_arrays.iter_mut().map(std::ptr::from_mut).collect();
    }

    /// Prepares the buffers and children (the `initialize` of each append data).
    fn initialize(&mut self, ty: &LogicalType, capacity: usize) -> Result<()> {
        match self.kind {
            AppendKind::Null | AppendKind::MapEntries => {}
            AppendKind::Bool => self.main_buffer().reserve(capacity.div_ceil(8)),
            AppendKind::Scalar(op) => {
                let width = scalar_width(op, ty.physical_type());
                self.main_buffer().reserve(capacity * width);
            }
            AppendKind::Varchar { large } | AppendKind::UuidText { large } => {
                let offset_width = if large { 8 } else { 4 };
                self.main_buffer().reserve((capacity + 1) * offset_width);
                self.aux_buffer().reserve(capacity);
            }
            AppendKind::StringView => {
                self.main_buffer().reserve(capacity * 16);
                self.aux_buffer().reserve(capacity);
                self.arrow_buffers[3].reserve(size_of::<i64>());
            }
            AppendKind::Enum => {
                self.main_buffer().reserve(capacity * ty.physical_type().size());
                // The dictionary is always written as regular (non-view, int32 offset)
                // strings, whatever the settings.
                let mut options = self.options.clone();
                options.produce_arrow_string_view = false;
                options.arrow_offset_size = ArrowOffsetSize::Regular;
                let values = ty.enum_values().unwrap_or_default();
                let mut enum_data = Self::initialize_child(&LogicalType::VARCHAR, values.len(), &options)?;
                enum_data.append_enum_values(values);
                self.child_data.push(enum_data);
            }
            AppendKind::Struct => {
                for (_, child_type) in nested_children(ty, "STRUCT")? {
                    let child = Self::initialize_child(child_type, capacity, &self.options)?;
                    self.child_data.push(child);
                }
            }
            AppendKind::Union => {
                self.main_buffer().reserve(capacity);
                for (_, member_type) in nested_children(ty, "UNION")? {
                    let child = Self::initialize_child(member_type, capacity, &self.options)?;
                    self.child_data.push(child);
                }
            }
            AppendKind::FixedSizeList => {
                let (child_type, size) = ty
                    .array_child()
                    .ok_or_else(|| Error::internal("ARRAY type without a child type"))?;
                let child = Self::initialize_child(child_type, capacity * size as usize, &self.options)?;
                self.child_data.push(child);
            }
            AppendKind::List { large } => {
                let offset_width = if large { 8 } else { 4 };
                self.main_buffer().reserve((capacity + 1) * offset_width);
                let child = Self::initialize_child(list_child_type(ty)?, capacity, &self.options)?;
                self.child_data.push(child);
            }
            AppendKind::ListView { large } => {
                let offset_width = if large { 8 } else { 4 };
                self.main_buffer().reserve(capacity * offset_width);
                self.aux_buffer().reserve(capacity * offset_width);
                let child = Self::initialize_child(list_child_type(ty)?, capacity, &self.options)?;
                self.child_data.push(child);
            }
            AppendKind::Map => {
                // The main buffer holds the offsets; the single child is a struct of the
                // keys and the values.
                self.main_buffer().reserve((capacity + 1) * size_of::<i32>());
                let (key_type, value_type) = ty
                    .map_key_value()
                    .ok_or_else(|| Error::internal("MAP type without key and value types"))?;
                let mut entries = Self::new(AppendKind::MapEntries, self.options.clone());
                entries
                    .child_data
                    .push(Self::initialize_child(key_type, capacity, &self.options)?);
                entries
                    .child_data
                    .push(Self::initialize_child(value_type, capacity, &self.options)?);
                self.child_data.push(entries);
            }
        }
        Ok(())
    }

    /// Appends rows `from..to` of `input` (of `input_size` rows), converted to the
    /// extension's internal type first if the column has one (`AppendChild`).
    fn append_child(&mut self, input: &Vector, from: usize, to: usize, input_size: usize) -> Result<()> {
        if let Some(extension) = &self.extension_data
            && let Some(duckdb_to_arrow) = extension.duckdb_to_arrow
        {
            // Container children can have more than `STANDARD_VECTOR_SIZE` rows.
            let mut internal = Vector::flat(
                extension.internal_type().clone(),
                input_size.max(STANDARD_VECTOR_SIZE),
            );
            duckdb_to_arrow(input, &mut internal, input_size)?;
            return self.append_vector(&internal, from, to, input_size);
        }
        self.append_vector(input, from, to, input_size)
    }

    /// Marks row `row` NULL (`SetNull`).
    fn set_null(&mut self, row: usize) {
        let (byte, bit) = bit_position(row);
        self.validity().bytes_mut()[byte] &= !(1 << bit);
        self.null_count += 1;
    }

    /// Grows a bitmap to `row_count` rows, new bits set (`ResizeValidity`).
    fn resize_bitmap(buffer: &mut ArrowBuffer, row_count: usize) {
        buffer.resize_with(row_count.div_ceil(8), 0xFF);
    }

    /// Appends the validity of rows `from..to` (`AppendValidity`).
    fn append_validity(&mut self, validity: &ValidityMask, sel: Sel<'_>, from: usize, to: usize) {
        let row_count = self.row_count;
        Self::resize_bitmap(self.validity(), row_count + to - from);
        if validity.is_all_valid() {
            return;
        }
        for row in from..to {
            if !validity.is_valid(sel.slot(row)) {
                self.set_null(row_count + row - from);
            }
        }
    }

    /// Appends rows `from..to` of `input` in the column's layout (the `append_vector` of
    /// each append data).
    fn append_vector(&mut self, input: &Vector, from: usize, to: usize, input_size: usize) -> Result<()> {
        match self.kind {
            AppendKind::Null => self.row_count += to - from,
            AppendKind::Bool => self.append_bool(input, from, to, input_size),
            AppendKind::Scalar(op) => self.append_scalar_op(op, input, from, to, input_size)?,
            AppendKind::Enum => self.append_scalar_op(ScalarOp::Copy, input, from, to, input_size)?,
            AppendKind::Varchar { large: true } => {
                self.append_varchar::<i64>(input, from, to, input_size, false)?;
            }
            AppendKind::Varchar { large: false } => {
                self.append_varchar::<i32>(input, from, to, input_size, false)?;
            }
            AppendKind::UuidText { large: true } => {
                self.append_varchar::<i64>(input, from, to, input_size, true)?;
            }
            AppendKind::UuidText { large: false } => {
                self.append_varchar::<i32>(input, from, to, input_size, true)?;
            }
            AppendKind::StringView => self.append_string_view(input, from, to, input_size),
            AppendKind::Struct => self.append_struct(input, from, to, input_size)?,
            AppendKind::Union => self.append_union(input, from, to)?,
            AppendKind::FixedSizeList => self.append_fixed_size_list(input, from, to, input_size)?,
            AppendKind::List { large: true } => self.append_list::<i64>(input, from, to, input_size)?,
            AppendKind::List { large: false } => self.append_list::<i32>(input, from, to, input_size)?,
            AppendKind::ListView { large: true } => {
                self.append_list_view::<i64>(input, from, to, input_size)?;
            }
            AppendKind::ListView { large: false } => {
                self.append_list_view::<i32>(input, from, to, input_size)?;
            }
            AppendKind::Map => self.append_map(input, from, to, input_size)?,
            AppendKind::MapEntries => return Err(Error::internal("map entries are appended by their map")),
        }
        Ok(())
    }

    /// `ArrowBoolData::Append`: values and validity bit-packed.
    fn append_bool(&mut self, input: &Vector, from: usize, to: usize, input_size: usize) {
        let input = readable(input, input_size);
        let format = input.unified::<bool>();
        let row_count = self.row_count;
        let size = to - from;
        // Both bitmaps start out all set.
        Self::resize_bitmap(self.validity(), row_count + size);
        Self::resize_bitmap(self.main_buffer(), row_count + size);
        for row in from..to {
            let slot = format.sel.slot(row);
            let target = row_count + row - from;
            if !format.validity.is_valid(slot) {
                self.set_null(target);
            } else if !format.values[slot] {
                let (byte, bit) = bit_position(target);
                self.main_buffer().bytes_mut()[byte] &= !(1 << bit);
            }
        }
        self.row_count += size;
    }

    /// `ArrowScalarData::Append` of the values of `input` converted by `op`.
    fn append_scalar_op(
        &mut self,
        op: ScalarOp,
        input: &Vector,
        from: usize,
        to: usize,
        input_size: usize,
    ) -> Result<()> {
        let input = readable(input, input_size);
        let input = input.as_ref();
        match (op, input.logical_type().physical_type()) {
            (ScalarOp::Copy, PhysicalType::Int8) => self.append_scalar(input, from, to, false, |v: i8| v),
            (ScalarOp::Copy, PhysicalType::Int16) => self.append_scalar(input, from, to, false, |v: i16| v),
            (ScalarOp::Copy, PhysicalType::Int32) => self.append_scalar(input, from, to, false, |v: i32| v),
            (ScalarOp::Copy, PhysicalType::Int64) => self.append_scalar(input, from, to, false, |v: i64| v),
            (ScalarOp::Copy, PhysicalType::Int128) => self.append_scalar(input, from, to, false, |v: i128| v),
            (ScalarOp::Copy, PhysicalType::UInt8) => self.append_scalar(input, from, to, false, |v: u8| v),
            (ScalarOp::Copy, PhysicalType::UInt16) => self.append_scalar(input, from, to, false, |v: u16| v),
            (ScalarOp::Copy, PhysicalType::UInt32) => self.append_scalar(input, from, to, false, |v: u32| v),
            (ScalarOp::Copy, PhysicalType::UInt64) => self.append_scalar(input, from, to, false, |v: u64| v),
            (ScalarOp::Copy, PhysicalType::UInt128) => {
                self.append_scalar(input, from, to, false, |v: u128| v);
            }
            (ScalarOp::Copy, PhysicalType::Float) => self.append_scalar(input, from, to, false, |v: f32| v),
            (ScalarOp::Copy, PhysicalType::Double) => self.append_scalar(input, from, to, false, |v: f64| v),
            (ScalarOp::WidenTo32, PhysicalType::Int16) => {
                self.append_scalar(input, from, to, false, |v: i16| i32::from(v));
            }
            (ScalarOp::WidenTo128, PhysicalType::Int16) => {
                self.append_scalar(input, from, to, false, |v: i16| i128::from(v));
            }
            (ScalarOp::WidenTo128, PhysicalType::Int32) => {
                self.append_scalar(input, from, to, false, |v: i32| i128::from(v));
            }
            (ScalarOp::WidenTo128, PhysicalType::Int64) => {
                self.append_scalar(input, from, to, false, |v: i64| i128::from(v));
            }
            (ScalarOp::TimeTz, _) => self.append_scalar(input, from, to, true, |v: TimeTz| v.time().0),
            (ScalarOp::Interval, _) => {
                self.append_scalar(input, from, to, true, |v: Interval| ArrowInterval {
                    months: v.months,
                    days: v.days,
                    nanoseconds: v.micros.wrapping_mul(1000),
                });
            }
            (ScalarOp::UuidBlob, _) => self.append_scalar(input, from, to, true, |v: Uuid| v.to_blob()),
            (op, physical) => {
                return Err(Error::internal(format!(
                    "Arrow appender: cannot append {physical:?} values as {op:?}"
                )));
            }
        }
        Ok(())
    }

    /// `ArrowScalarBaseData::Append`: the values of rows `from..to` converted by `op`;
    /// with `skip_nulls` the slots of NULL rows are left zeroed.
    fn append_scalar<S: NativeType, T: Copy>(
        &mut self,
        input: &Vector,
        from: usize,
        to: usize,
        skip_nulls: bool,
        op: impl Fn(S) -> T,
    ) {
        let format = input.unified::<S>();
        self.append_validity(format.validity, format.sel, from, to);
        let row_count = self.row_count;
        let size = to - from;
        let main_buffer = self.main_buffer();
        main_buffer.resize(main_buffer.len() + size_of::<T>() * size);
        for row in from..to {
            let slot = format.sel.slot(row);
            if skip_nulls && !format.validity.is_valid(slot) {
                continue;
            }
            main_buffer.write(row_count + row - from, op(format.values[slot]));
        }
        self.row_count += size;
    }

    /// `ArrowVarcharData::Append`: offsets of type `O` into the concatenated strings (or
    /// UUIDs as text).
    fn append_varchar<O: OffsetType>(
        &mut self,
        input: &Vector,
        from: usize,
        to: usize,
        input_size: usize,
        uuid: bool,
    ) -> Result<()> {
        let input = readable(input, input_size);
        let input = input.as_ref();
        let sel = input.sel();
        let validity = input.validity();
        let row_count = self.row_count;
        let size = to - from;
        Self::resize_bitmap(self.validity(), row_count + size);
        let main_buffer = self.main_buffer();
        main_buffer.resize(main_buffer.len() + size_of::<O>() * (size + 1));
        if row_count == 0 {
            main_buffer.write(0, O::from_usize(0));
        }
        let mut last_offset = self.main_buffer().read::<O>(row_count).to_usize();
        let mut uuid_text = Vec::with_capacity(Uuid::STRING_SIZE);
        for row in from..to {
            let slot = sel.slot(row);
            let offset_index = row_count + row + 1 - from;
            if !validity.is_valid(slot) {
                self.set_null(row_count + row - from);
                self.main_buffer().write(offset_index, O::from_usize(last_offset));
                continue;
            }
            let bytes: &[u8] = if uuid {
                uuid_text.clear();
                Uuid(input.values::<i128>()[slot]).write_to(&mut uuid_text);
                &uuid_text
            } else {
                input.string_value(&input.values::<StringT>()[slot])
            };
            let current_offset = last_offset + bytes.len();
            if !O::LARGE && current_offset > i32::MAX as usize {
                return Err(Error::invalid_input(format!(
                    "Arrow Appender: The maximum total string size for regular string buffers is {} but the \
                     offset of {current_offset} exceeds this.\n* SET arrow_large_buffer_size=true to use large \
                     string buffers",
                    i32::MAX
                )));
            }
            self.main_buffer()
                .write(offset_index, O::from_usize(current_offset));
            let aux_buffer = self.aux_buffer();
            aux_buffer.resize(current_offset);
            aux_buffer.bytes_mut()[last_offset..current_offset].copy_from_slice(bytes);
            last_offset = current_offset;
        }
        self.row_count += size;
        Ok(())
    }

    /// `ArrowVarcharToStringViewData::Append`: 16-byte views, strings of more than 12
    /// bytes in the single data buffer.
    fn append_string_view(&mut self, input: &Vector, from: usize, to: usize, input_size: usize) {
        const MAX_INLINED_BYTES: usize = 12;
        let input = readable(input, input_size);
        let input = input.as_ref();
        let sel = input.sel();
        let validity = input.validity();
        let row_count = self.row_count;
        let size = to - from;
        Self::resize_bitmap(self.validity(), row_count + size);
        let main_buffer = self.main_buffer();
        main_buffer.resize(main_buffer.len() + 16 * size);
        for row in from..to {
            let result_index = row_count + row - from;
            let slot = sel.slot(row);
            // A NULL row has an all-zero view.
            let mut view = [0u8; 16];
            if validity.is_valid(slot) {
                let bytes = input.string_value(&input.values::<StringT>()[slot]);
                view[..4].copy_from_slice(&(bytes.len() as i32).to_le_bytes());
                if bytes.len() <= MAX_INLINED_BYTES {
                    // | length | data (zero-padded) |
                    view[4..4 + bytes.len()].copy_from_slice(bytes);
                } else {
                    // | length | prefix | buffer index (0) | offset |
                    view[4..8].copy_from_slice(&bytes[..4]);
                    view[12..].copy_from_slice(&(self.offset as i32).to_le_bytes());
                    let current_offset = self.offset + bytes.len();
                    let aux_buffer = self.aux_buffer();
                    aux_buffer.resize(current_offset);
                    aux_buffer.bytes_mut()[current_offset - bytes.len()..].copy_from_slice(bytes);
                    self.offset = current_offset;
                }
            } else {
                self.set_null(result_index);
            }
            self.main_buffer().write(result_index, view);
        }
        self.row_count += size;
    }

    /// `ArrowEnumData::EnumAppendVector`: the enum's values as the strings of the
    /// dictionary.
    fn append_enum_values(&mut self, values: &[rustdb_common::ByteString]) {
        let size = values.len();
        let row_count = self.row_count;
        Self::resize_bitmap(self.validity(), row_count + size);
        let main_buffer = self.main_buffer();
        main_buffer.resize(main_buffer.len() + size_of::<i32>() * (size + 1));
        if row_count == 0 {
            main_buffer.write(0, 0i32);
        }
        let mut last_offset = self.main_buffer().read::<i32>(row_count) as usize;
        for (index, value) in values.iter().enumerate() {
            let current_offset = last_offset + value.len();
            self.main_buffer()
                .write(row_count + index + 1, current_offset as i32);
            let aux_buffer = self.aux_buffer();
            aux_buffer.resize(current_offset);
            aux_buffer.bytes_mut()[last_offset..current_offset].copy_from_slice(value.as_bytes());
            last_offset = current_offset;
        }
        self.row_count += size;
    }

    /// `ArrowStructData::Append`
    fn append_struct(&mut self, input: &Vector, from: usize, to: usize, input_size: usize) -> Result<()> {
        let input = flat(input, input_size);
        self.append_validity(input.validity(), Sel::Identity, from, to);
        let children = input.struct_children().unwrap_or_default();
        for (child_data, child) in self.child_data.iter_mut().zip(children) {
            child_data.append_child(child, from, to, input_size)?;
        }
        self.row_count += to - from;
        Ok(())
    }

    /// `ArrowUnionData::Append`: a sparse union, every member as long as the union.
    fn append_union(&mut self, input: &Vector, from: usize, to: usize) -> Result<()> {
        let size = to - from;
        let member_types: Vec<LogicalType> = input
            .logical_type()
            .union_members()
            .unwrap_or_default()
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect();
        let mut child_vectors: Vec<Vector> = member_types
            .iter()
            .map(|ty| Vector::flat(ty.clone(), size))
            .collect();
        for row in 0..size {
            let value = input.get_value(from + row);
            let (tag, member) = match value.union_tag_and_value() {
                Some((tag, member)) if !value.is_null() => (usize::from(tag), Some(member.clone())),
                _ => (0, None),
            };
            for (index, (child, ty)) in child_vectors.iter_mut().zip(&member_types).enumerate() {
                match &member {
                    Some(member) if index == tag => child.set_value(row, member)?,
                    _ => child.set_value(row, &rustdb_types::Value::null(ty.clone()))?,
                }
            }
            self.main_buffer().push(tag as u8);
        }
        for (child_data, child) in self.child_data.iter_mut().zip(&child_vectors) {
            child_data.append_child(child, 0, size, size)?;
        }
        self.row_count += size;
        Ok(())
    }

    /// `ArrowFixedSizeListData::Append`
    fn append_fixed_size_list(
        &mut self,
        input: &Vector,
        from: usize,
        to: usize,
        input_size: usize,
    ) -> Result<()> {
        let input = flat(input, input_size);
        self.append_validity(input.validity(), Sel::Identity, from, to);
        let size = to - from;
        let array_size = input
            .logical_type()
            .array_child()
            .map_or(0, |(_, size)| size as usize);
        let child = input
            .array_child()
            .ok_or_else(|| Error::internal("ARRAY vector without a child vector"))?;
        self.child_data[0].append_child(child, from * array_size, to * array_size, size * array_size)?;
        self.row_count += size;
        Ok(())
    }

    /// `ArrowListData::AppendOffsets`: the offsets of rows `from..to`, and the child
    /// rows of their elements in `child_sel`.
    fn append_offsets<O: OffsetType>(
        &mut self,
        entries: &rustdb_types::Unified<'_, ListEntry>,
        from: usize,
        to: usize,
        child_sel: &mut Vec<u32>,
    ) -> Result<()> {
        let row_count = self.row_count;
        let size = to - from;
        let main_buffer = self.main_buffer();
        main_buffer.resize(main_buffer.len() + size_of::<O>() * (size + 1));
        if row_count == 0 {
            main_buffer.write(0, O::from_usize(0));
        }
        let mut last_offset = main_buffer.read::<O>(row_count).to_usize();
        for row in from..to {
            let slot = entries.sel.slot(row);
            let offset_index = row_count + row + 1 - from;
            if !entries.validity.is_valid(slot) {
                main_buffer.write(offset_index, O::from_usize(last_offset));
                continue;
            }
            let entry = entries.values[slot];
            let length = entry.length as usize;
            if !O::LARGE && last_offset + length > i32::MAX as usize {
                return Err(list_offset_overflow(last_offset));
            }
            last_offset += length;
            main_buffer.write(offset_index, O::from_usize(last_offset));
            child_sel.extend((0..length).map(|k| (entry.offset as usize + k) as u32));
        }
        Ok(())
    }

    /// `ArrowListData::Append`
    fn append_list<O: OffsetType>(
        &mut self,
        input: &Vector,
        from: usize,
        to: usize,
        input_size: usize,
    ) -> Result<()> {
        let input = readable(input, input_size);
        let entries = input.unified::<ListEntry>();
        self.append_validity(entries.validity, entries.sel, from, to);
        let mut child_sel = Vec::new();
        self.append_offsets::<O>(&entries, from, to, &mut child_sel)?;
        self.append_list_child(&input, child_sel)?;
        self.row_count += to - from;
        Ok(())
    }

    /// Appends the elements at `child_sel` of the list vector `input` to the child.
    fn append_list_child(&mut self, input: &Vector, child_sel: Vec<u32>) -> Result<()> {
        let (child, _) = input
            .list_child()
            .ok_or_else(|| Error::internal("LIST vector without a child vector"))?;
        let child_size = child_sel.len();
        let child_copy = child.slice(&SelectionVector::new(child_sel));
        self.child_data[0].append_child(&child_copy, 0, child_size, child_size)
    }

    /// `ArrowListViewData::Append`: an offset and a size per row.
    fn append_list_view<O: OffsetType>(
        &mut self,
        input: &Vector,
        from: usize,
        to: usize,
        input_size: usize,
    ) -> Result<()> {
        let input = readable(input, input_size);
        let entries = input.unified::<ListEntry>();
        self.append_validity(entries.validity, entries.sel, from, to);
        // `AppendListMetadata`
        let row_count = self.row_count;
        let size = to - from;
        for buffer in [1, 2] {
            let buffer = &mut self.arrow_buffers[buffer];
            buffer.resize(buffer.len() + size_of::<O>() * size);
        }
        let mut last_offset = if row_count == 0 {
            0
        } else {
            self.main_buffer().read::<O>(row_count - 1).to_usize()
                + self.aux_buffer().read::<O>(row_count - 1).to_usize()
        };
        let mut child_sel = Vec::new();
        for index in 0..size {
            let slot = entries.sel.slot(from + index);
            let offset_index = row_count + index;
            if !entries.validity.is_valid(slot) {
                self.main_buffer().write(offset_index, O::from_usize(last_offset));
                self.aux_buffer().write(offset_index, O::from_usize(0));
                continue;
            }
            let entry = entries.values[slot];
            let length = entry.length as usize;
            if !O::LARGE && last_offset + length > i32::MAX as usize {
                return Err(list_offset_overflow(last_offset));
            }
            self.main_buffer().write(offset_index, O::from_usize(last_offset));
            self.aux_buffer().write(offset_index, O::from_usize(length));
            last_offset += length;
            child_sel.extend((0..length).map(|k| (entry.offset as usize + k) as u32));
        }
        self.append_list_child(&input, child_sel)?;
        self.row_count += size;
        Ok(())
    }

    /// `ArrowMapData::Append`: list offsets, and the keys and values of the entries.
    fn append_map(&mut self, input: &Vector, from: usize, to: usize, input_size: usize) -> Result<()> {
        let input = readable(input, input_size);
        let entries = input.unified::<ListEntry>();
        self.append_validity(entries.validity, entries.sel, from, to);
        let mut child_sel = Vec::new();
        self.append_offsets::<i32>(&entries, from, to, &mut child_sel)?;
        let (child, child_len) = input
            .list_child()
            .ok_or_else(|| Error::internal("MAP vector without a child vector"))?;
        let child = flat(child, child_len);
        let fields = child.struct_children().unwrap_or_default();
        let (Some(keys), Some(values)) = (fields.first(), fields.get(1)) else {
            return Err(Error::internal("MAP vector without keys and values"));
        };
        let list_size = child_sel.len();
        let sel = SelectionVector::new(child_sel);
        let size = to - from;
        let entries_data = &mut self.child_data[0];
        entries_data.child_data[0].append_child(&keys.slice(&sel), 0, list_size, list_size)?;
        entries_data.child_data[1].append_child(&values.slice(&sel), 0, list_size, list_size)?;
        entries_data.row_count += size;
        self.row_count += size;
        Ok(())
    }

    /// Finalizes the children of a nested type into `result` (the child part of the
    /// `finalize` of struct, union, list and map append data).
    fn finalize_children(&mut self, child_types: &[&LogicalType], result: &mut ArrowArray) -> Result<()> {
        self.add_children(child_types.len());
        result.children = self.child_pointers.as_mut_ptr();
        result.n_children = child_types.len() as i64;
        for (index, (ty, data)) in child_types
            .iter()
            .zip(std::mem::take(&mut self.child_data))
            .enumerate()
        {
            self.child_arrays[index] = finalize_child(ty, data)?;
        }
        Ok(())
    }

    /// Sets the buffers and children of the finalized array (the `finalize` of each
    /// append data).
    fn finalize(&mut self, ty: &LogicalType, result: &mut ArrowArray) -> Result<()> {
        match self.kind {
            AppendKind::Null => result.n_buffers = 0,
            AppendKind::Bool | AppendKind::Scalar(_) => {
                result.n_buffers = 2;
                self.buffers[1] = self.arrow_buffers[1].data();
            }
            AppendKind::Enum => {
                result.n_buffers = 2;
                self.buffers[1] = self.arrow_buffers[1].data();
                let values = self
                    .child_data
                    .pop()
                    .ok_or_else(|| Error::internal("ENUM append data without its values"))?;
                self.dictionary = finalize_child(&LogicalType::VARCHAR, values)?;
                result.dictionary = &raw mut self.dictionary;
            }
            AppendKind::Varchar { .. } | AppendKind::UuidText { .. } => {
                result.n_buffers = 3;
                self.buffers[1] = self.arrow_buffers[1].data();
                self.buffers[2] = self.arrow_buffers[2].data();
            }
            AppendKind::StringView => {
                // Validity, views, the one data buffer and the sizes of the data buffers.
                result.n_buffers = 4;
                self.buffers[1] = self.arrow_buffers[1].data();
                self.buffers[2] = self.arrow_buffers[2].data();
                let sizes = &mut self.arrow_buffers[3];
                sizes.resize(size_of::<i64>());
                sizes.write(0, self.offset as i64);
                self.buffers[3] = sizes.data();
            }
            AppendKind::Struct => {
                result.n_buffers = 1;
                let children = nested_children(ty, "STRUCT")?;
                self.finalize_children(&children.iter().map(|(_, ty)| ty).collect::<Vec<_>>(), result)?;
            }
            AppendKind::Union => {
                // Unions have no validity: the first buffer holds the type ids.
                result.n_buffers = 1;
                self.buffers[0] = self.arrow_buffers[1].data();
                let members = nested_children(ty, "UNION")?;
                self.finalize_children(&members.iter().map(|(_, ty)| ty).collect::<Vec<_>>(), result)?;
            }
            AppendKind::FixedSizeList => {
                result.n_buffers = 1;
                let (child_type, _) = ty
                    .array_child()
                    .ok_or_else(|| Error::internal("ARRAY type without a child type"))?;
                self.finalize_children(&[child_type], result)?;
            }
            AppendKind::List { .. } | AppendKind::Map => {
                result.n_buffers = 2;
                self.buffers[1] = self.arrow_buffers[1].data();
                self.finalize_children(&[list_child_type(ty)?], result)?;
            }
            AppendKind::ListView { .. } => {
                result.n_buffers = 3;
                self.buffers[1] = self.arrow_buffers[1].data();
                self.buffers[2] = self.arrow_buffers[2].data();
                self.finalize_children(&[list_child_type(ty)?], result)?;
            }
            AppendKind::MapEntries => {
                // As long as the keys, which may not be NULL.
                result.n_buffers = 1;
                result.length = self.child_data.first().map_or(0, |keys| keys.row_count) as i64;
                let fields = nested_children(ty, "MAP entries")?;
                self.finalize_children(&fields.iter().map(|(_, ty)| ty).collect::<Vec<_>>(), result)?;
                if self.child_arrays[0].null_count > 0 {
                    return Err(Error::invalid_input("Arrow doesn't accept NULL keys on Maps"));
                }
            }
        }
        Ok(())
    }
}

/// The error of a list whose offsets exceed 32 bits.
fn list_offset_overflow(last_offset: usize) -> Error {
    Error::invalid_input(format!(
        "Arrow Appender: The maximum combined list offset for regular list buffers is {} but the offset of \
         {last_offset} exceeds this.\n* SET arrow_large_buffer_size=true to use large list buffers",
        i32::MAX
    ))
}

/// The offset type of a variable-size layout (`BUFTYPE`): `i32`, or `i64` for the large
/// layouts.
trait OffsetType: Copy {
    const LARGE: bool;
    fn from_usize(value: usize) -> Self;
    fn to_usize(self) -> usize;
}

impl OffsetType for i32 {
    const LARGE: bool = false;
    fn from_usize(value: usize) -> Self {
        value as Self
    }
    fn to_usize(self) -> usize {
        self as usize
    }
}

impl OffsetType for i64 {
    const LARGE: bool = true;
    fn from_usize(value: usize) -> Self {
        value as Self
    }
    fn to_usize(self) -> usize {
        self as usize
    }
}
