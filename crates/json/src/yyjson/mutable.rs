//! Mutable JSON values (`yyjson_mut_val`) and RFC 7386 merge patches
//! (`yyjson_mut_merge_patch`).

use super::{Doc, ValId, ValType, tag};

/// A JSON value that owns its contents. Objects keep their keys in insertion order and may
/// hold duplicate keys, like yyjson.
#[derive(Clone, Debug, PartialEq)]
pub enum MutVal {
    Null,
    Bool(bool),
    UInt(u64),
    SInt(i64),
    Real(f64),
    /// A number kept as its text (bignums, `NaN`, `Infinity`).
    Raw(Vec<u8>),
    Str(Vec<u8>),
    Arr(Vec<MutVal>),
    Obj(Vec<(Vec<u8>, MutVal)>),
}

impl MutVal {
    /// A deep copy of value `v` of `doc` (`yyjson_val_mut_copy`).
    pub fn from_doc(doc: &Doc<'_>, v: ValId) -> Self {
        match doc.get_type(v) {
            ValType::Raw => Self::Raw(doc.str(v).to_vec()),
            ValType::Str => Self::Str(doc.str(v).to_vec()),
            ValType::Num => match doc.tag(v) {
                tag::REAL => Self::Real(doc.get_real(v)),
                tag::SINT => Self::SInt(doc.get_sint(v)),
                _ => Self::UInt(doc.get_uint(v)),
            },
            ValType::Bool => Self::Bool(doc.get_bool(v)),
            ValType::Null | ValType::None => Self::Null,
            ValType::Arr => Self::Arr(doc.arr_iter(v).map(|item| Self::from_doc(doc, item)).collect()),
            ValType::Obj => Self::Obj(
                doc.obj_iter(v)
                    .map(|(key, value)| (doc.str(key).to_vec(), Self::from_doc(doc, value)))
                    .collect(),
            ),
        }
    }

    /// The value of the first key equal to `key` (`yyjson_mut_obj_getn`).
    pub fn obj_get(&self, key: &[u8]) -> Option<&MutVal> {
        match self {
            Self::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, value)| value),
            _ => None,
        }
    }

    /// Appends the value as minified JSON.
    pub fn write(&self, out: &mut Vec<u8>) {
        super::write::write_mut(out, self);
    }
}

/// `yyjson_mut_merge_patch`: applies `patch` to `orig` (RFC 7386).
pub fn merge_patch(orig: Option<&MutVal>, patch: &MutVal) -> MutVal {
    let MutVal::Obj(patch_pairs) = patch else {
        return patch.clone();
    };
    let mut builder = Vec::new();
    let orig = orig.filter(|orig| matches!(orig, MutVal::Obj(_)));
    if let Some(MutVal::Obj(orig_pairs)) = orig {
        for (key, value) in orig_pairs {
            if patch.obj_get(key).is_none() {
                builder.push((key.clone(), value.clone()));
            }
        }
    }
    for (key, patch_value) in patch_pairs {
        if matches!(patch_value, MutVal::Null) {
            continue;
        }
        let orig_value = orig.and_then(|orig| orig.obj_get(key));
        builder.push((key.clone(), merge_patch(orig_value, patch_value)));
    }
    MutVal::Obj(builder)
}
