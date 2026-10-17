//! A port of the parts of yyjson 0.10 (`third_party/yyjson`) that DuckDB's JSON extension
//! uses: the reader with DuckDB's flags (`ALLOW_INF_AND_NAN | ALLOW_TRAILING_COMMAS |
//! BIGNUM_AS_RAW`, optionally `STOP_WHEN_DONE`), the immutable document layout, the
//! minified and pretty writers, JSON pointers, equality and the mutable values used by
//! `json_merge_patch`.
//!
//! Documents use yyjson's layout: every value is one 16-byte [`Val`] slot in pre-order,
//! a container is followed by its children (an object by key/value pairs) and records the
//! number of slots of its subtree, so walking a document is a linear scan. Strings are
//! unescaped in place in the parsed bytes and referenced by offset.

mod mutable;
mod pointer;
mod read;
mod write;

pub use mutable::{MutVal, merge_patch};
pub use pointer::ptr_get;
pub use read::{JsonArena, ReadError, ReadErrorCode, ReadResult, read_insitu};
pub use write::{
    write_f64, write_i64, write_mut_pretty, write_raw, write_string, write_u64, write_val, write_val_pretty,
};

/// The zero bytes yyjson requires after the input (`YYJSON_PADDING_SIZE`).
pub const PADDING_SIZE: usize = 4;

/// `YYJSON_TYPE_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ValType {
    None = 0,
    Raw = 1,
    Null = 2,
    Bool = 3,
    Num = 4,
    Str = 5,
    Arr = 6,
    Obj = 7,
}

pub(crate) const TYPE_MASK: u64 = 0x07;
pub(crate) const TYPE_RAW: u64 = 1;
pub(crate) const TYPE_NULL: u64 = 2;
pub(crate) const TYPE_BOOL: u64 = 3;
pub(crate) const TYPE_NUM: u64 = 4;
pub(crate) const TYPE_STR: u64 = 5;
pub(crate) const TYPE_ARR: u64 = 6;
pub(crate) const TYPE_OBJ: u64 = 7;
pub(crate) const SUBTYPE_TRUE: u64 = 1 << 3;
pub(crate) const SUBTYPE_UINT: u64 = 0;
pub(crate) const SUBTYPE_SINT: u64 = 1 << 3;
pub(crate) const SUBTYPE_REAL: u64 = 2 << 3;
pub(crate) const SUBTYPE_NOESC: u64 = 1 << 3;
pub(crate) const TAG_MASK: u64 = 0xFF;
pub(crate) const TAG_BIT: u32 = 8;

/// The tags DuckDB switches on (`yyjson_get_tag`): type and subtype.
pub mod tag {
    pub const NULL: u8 = 2;
    pub const FALSE: u8 = 3;
    pub const TRUE: u8 = 3 | (1 << 3);
    pub const UINT: u8 = 4;
    pub const SINT: u8 = 4 | (1 << 3);
    pub const REAL: u8 = 4 | (2 << 3);
    pub const RAW: u8 = 1;
    pub const STR: u8 = 5;
    pub const STR_NOESC: u8 = 5 | (1 << 3);
    pub const ARR: u8 = 6;
    pub const OBJ: u8 = 7;
}

/// One value slot (`yyjson_val`): the tag (type, subtype and length) and a payload that is
/// the number, the offset of the string bytes, or for containers the number of slots of
/// the subtree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Val {
    pub(crate) tag: u64,
    pub(crate) uni: u64,
}

/// The index of a value slot in a document.
pub type ValId = usize;

/// A parsed document: its value slots and the bytes its strings live in.
#[derive(Clone, Copy, Debug)]
pub struct Doc<'a> {
    vals: &'a [Val],
    bytes: &'a [u8],
}

impl<'a> Doc<'a> {
    pub fn new(vals: &'a [Val], bytes: &'a [u8]) -> Self {
        Self { vals, bytes }
    }

    #[inline]
    fn val(&self, v: ValId) -> Val {
        self.vals[v]
    }

    /// The type and subtype (`yyjson_get_tag`).
    #[inline]
    pub fn tag(&self, v: ValId) -> u8 {
        (self.val(v).tag & TAG_MASK) as u8
    }

    /// `yyjson_get_type`.
    #[inline]
    pub fn get_type(&self, v: ValId) -> ValType {
        match self.val(v).tag & TYPE_MASK {
            TYPE_RAW => ValType::Raw,
            TYPE_NULL => ValType::Null,
            TYPE_BOOL => ValType::Bool,
            TYPE_NUM => ValType::Num,
            TYPE_STR => ValType::Str,
            TYPE_ARR => ValType::Arr,
            TYPE_OBJ => ValType::Obj,
            _ => ValType::None,
        }
    }

    /// The length of a string, or the number of elements / pairs of a container.
    #[inline]
    pub fn len(&self, v: ValId) -> usize {
        (self.val(v).tag >> TAG_BIT) as usize
    }

    #[inline]
    pub fn is_null(&self, v: ValId) -> bool {
        self.val(v).tag & TYPE_MASK == TYPE_NULL
    }

    #[inline]
    pub fn is_obj(&self, v: ValId) -> bool {
        self.val(v).tag & TYPE_MASK == TYPE_OBJ
    }

    #[inline]
    pub fn is_arr(&self, v: ValId) -> bool {
        self.val(v).tag & TYPE_MASK == TYPE_ARR
    }

    #[inline]
    pub fn is_str(&self, v: ValId) -> bool {
        self.val(v).tag & TYPE_MASK == TYPE_STR
    }

    /// Whether the value is an array or object.
    #[inline]
    pub fn is_ctn(&self, v: ValId) -> bool {
        self.val(v).tag & 0x06 == 0x06
    }

    /// The bytes of a string or raw value.
    #[inline]
    pub fn str(&self, v: ValId) -> &'a [u8] {
        let val = self.val(v);
        let start = val.uni as usize;
        &self.bytes[start..start + (val.tag >> TAG_BIT) as usize]
    }

    #[inline]
    pub fn get_bool(&self, v: ValId) -> bool {
        self.val(v).tag & SUBTYPE_TRUE != 0
    }

    #[inline]
    pub fn get_uint(&self, v: ValId) -> u64 {
        self.val(v).uni
    }

    #[inline]
    pub fn get_sint(&self, v: ValId) -> i64 {
        self.val(v).uni as i64
    }

    #[inline]
    pub fn get_real(&self, v: ValId) -> f64 {
        f64::from_bits(self.val(v).uni)
    }

    /// The raw payload (the bits of a number).
    #[inline]
    pub(crate) fn uni(&self, v: ValId) -> u64 {
        self.val(v).uni
    }

    /// The value after `v` and its subtree (`unsafe_yyjson_get_next`).
    #[inline]
    pub fn next(&self, v: ValId) -> ValId {
        let val = self.val(v);
        if val.tag & 0x06 == 0x06 {
            v + val.uni as usize
        } else {
            v + 1
        }
    }

    /// The number of slots of the subtree of `v`.
    #[inline]
    pub fn subtree_len(&self, v: ValId) -> usize {
        self.next(v) - v
    }

    /// The elements of an array.
    pub fn arr_iter(&self, arr: ValId) -> ArrIter<'a> {
        ArrIter {
            doc: *self,
            cur: arr + 1,
            remaining: self.len(arr),
        }
    }

    /// The key/value pairs of an object.
    pub fn obj_iter(&self, obj: ValId) -> ObjIter<'a> {
        ObjIter {
            doc: *self,
            cur: obj + 1,
            remaining: self.len(obj),
        }
    }

    /// The value of the first key equal to `key` (`yyjson_obj_getn`).
    pub fn obj_getn(&self, obj: ValId, key: &[u8]) -> Option<ValId> {
        if !self.is_obj(obj) {
            return None;
        }
        self.obj_iter(obj)
            .find(|&(k, _)| self.str(k) == key)
            .map(|(_, value)| value)
    }

    /// The element at `idx` (`yyjson_arr_get`).
    pub fn arr_get(&self, arr: ValId, idx: usize) -> Option<ValId> {
        if !self.is_arr(arr) || idx >= self.len(arr) {
            return None;
        }
        // A flat array (no nested containers) has its elements in consecutive slots.
        if self.subtree_len(arr) == self.len(arr) + 1 {
            return Some(arr + 1 + idx);
        }
        self.arr_iter(arr).nth(idx)
    }

    /// `yyjson_arr_size` (0 for non-arrays).
    pub fn arr_size(&self, v: ValId) -> usize {
        if self.is_arr(v) { self.len(v) } else { 0 }
    }

    /// `yyjson_obj_size` (0 for non-objects).
    pub fn obj_size(&self, v: ValId) -> usize {
        if self.is_obj(v) { self.len(v) } else { 0 }
    }

    /// Deep equality (`unsafe_yyjson_equals`).
    pub fn equals(&self, lhs: ValId, other: &Doc<'_>, rhs: ValId) -> bool {
        let lt = self.get_type(lhs);
        if lt != other.get_type(rhs) {
            return false;
        }
        match lt {
            ValType::Obj => {
                let len = self.len(lhs);
                if len != other.len(rhs) {
                    return false;
                }
                let mut iter = ObjKeyFinder::new(*other, rhs);
                for (key, value) in self.obj_iter(lhs) {
                    let Some(found) = iter.getn(self.str(key)) else {
                        return false;
                    };
                    if !self.equals(value, other, found) {
                        return false;
                    }
                }
                true
            }
            ValType::Arr => {
                let len = self.len(lhs);
                if len != other.len(rhs) {
                    return false;
                }
                self.arr_iter(lhs)
                    .zip(other.arr_iter(rhs))
                    .all(|(l, r)| self.equals(l, other, r))
            }
            ValType::Num => {
                let (ls, rs) = (self.tag(lhs), other.tag(rhs));
                let (lu, ru) = (self.uni(lhs), other.uni(rhs));
                if ls == rs {
                    return lu == ru;
                }
                match (ls, rs) {
                    (tag::SINT, tag::UINT) => (lu as i64) >= 0 && lu == ru,
                    (tag::UINT, tag::SINT) => (ru as i64) >= 0 && lu == ru,
                    _ => false,
                }
            }
            ValType::Raw | ValType::Str => self.str(lhs) == other.str(rhs),
            ValType::Null | ValType::Bool => self.tag(lhs) == other.tag(rhs),
            ValType::None => false,
        }
    }
}

/// Iterates the elements of an array.
#[derive(Clone, Debug)]
pub struct ArrIter<'a> {
    doc: Doc<'a>,
    cur: ValId,
    remaining: usize,
}

impl Iterator for ArrIter<'_> {
    type Item = ValId;

    #[inline]
    fn next(&mut self) -> Option<ValId> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let current = self.cur;
        self.cur = self.doc.next(current);
        Some(current)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

/// Iterates the key/value pairs of an object.
#[derive(Clone, Debug)]
pub struct ObjIter<'a> {
    doc: Doc<'a>,
    cur: ValId,
    remaining: usize,
}

impl Iterator for ObjIter<'_> {
    type Item = (ValId, ValId);

    #[inline]
    fn next(&mut self) -> Option<(ValId, ValId)> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let key = self.cur;
        self.cur = self.doc.next(key + 1);
        Some((key, key + 1))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

/// Successive key lookups that resume after the previous match (`yyjson_obj_iter_getn`).
#[derive(Debug)]
struct ObjKeyFinder<'a> {
    doc: Doc<'a>,
    obj: ValId,
    idx: usize,
    max: usize,
    cur: ValId,
}

impl<'a> ObjKeyFinder<'a> {
    fn new(doc: Doc<'a>, obj: ValId) -> Self {
        Self {
            doc,
            obj,
            idx: 0,
            max: doc.len(obj),
            cur: obj + 1,
        }
    }

    fn getn(&mut self, key: &[u8]) -> Option<ValId> {
        let mut idx = self.idx;
        let mut max = self.max;
        let mut cur = self.cur;
        if idx == max {
            idx = 0;
            cur = self.obj + 1;
        }
        while idx < max {
            idx += 1;
            let next = self.doc.next(cur + 1);
            if self.doc.str(cur) == key {
                self.idx = idx;
                self.cur = next;
                return Some(cur + 1);
            }
            cur = next;
            if idx == self.max && self.idx < self.max {
                idx = 0;
                max = self.idx;
                cur = self.obj + 1;
            }
        }
        None
    }
}

#[cfg(test)]
mod tests;
