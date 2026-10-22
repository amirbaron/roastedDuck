//! Property structs ([`Content::Properties`](crate::config::Content::Properties)): a
//! class's members as borrowed fields and the `SerializeObject` impl writing them, which
//! the hand-written `Serialize` impls of rustdb's own types fill in.
//!
//! Field types follow the member types: scalars and enums by value, strings as `&str`,
//! vectors as slices of what Rust containers hold (the value a C++ pointer points to),
//! pointers as `Option<&T>` (`&T` when `required`), other classes by reference to the
//! hand-written type holding them (`data_type`).

use std::fmt::Write as _;

use super::enums::variant_name;
use super::{DefaultExpr, Emitter};
use crate::Result;
use crate::cpp_type::{CppType, has_default_by_default};
use crate::model::{Class, Member};
use crate::names::is_keyword;
use crate::spec::MemberStatus;

/// How a field reaches the serializer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// Held by value: written as `&self.field`.
    Value,
    /// Held by reference: written as `self.field`.
    Reference,
}

impl Emitter<'_> {
    pub fn emit_properties(&mut self, class: &Class) -> Result<()> {
        if class.config.custom || class.members.is_empty() {
            // `custom_implementation`: the Rust type's serialization is hand-written too;
            // a subclass without members writes nothing after its base.
            return Ok(());
        }
        let mut fields = String::new();
        let mut body = String::new();
        for member in &class.members {
            let written = member.status == MemberStatus::Existing || member.version != 1;
            if !written {
                let _ = writeln!(
                    body,
                    "        // `{}` ({}) is no longer written.",
                    member.spec.name, member.spec.id
                );
                continue;
            }
            if is_keyword(&member.rust_name) {
                return Err(format!("member {}: Rust keyword, add a rename", member.spec.name).into());
            }
            let (ty, pass) = self.property_type(member)?;
            let _ = write!(fields, "{}", Self::field_doc(member));
            let _ = writeln!(fields, "    pub {}: {ty},", member.rust_name);
            let stmt = self.write_property_member(member, pass)?;
            let _ = match (member.status, member.version) {
                (MemberStatus::Existing, 1) => writeln!(body, "        {stmt}"),
                (MemberStatus::Existing, version) => writeln!(
                    body,
                    "        if serializer.should_serialize({version}) {{\n            {stmt}\n        }}"
                ),
                (_, version) => writeln!(
                    body,
                    "        if !serializer.should_serialize({version}) {{\n            {stmt}\n        }}"
                ),
            };
        }
        let lifetime = if fields.contains("'a") { "<'a>" } else { "" };
        let doc = match (&class.spec.base, &class.spec.enum_values) {
            (Some(base), Some(values)) => format!(
                "/// The properties of DuckDB's `{}` (`{}`), written after those of `{base}`.",
                class.spec.class,
                values.to_vec().join("`, `")
            ),
            _ => format!(
                "/// The properties of DuckDB's `{}`, as its `Serialize` writes them.",
                class.spec.class
            ),
        };
        let _ = writeln!(self.body, "{doc}");
        let _ = writeln!(self.body, "#[derive(Clone, Copy, Debug)]");
        let _ = writeln!(self.body, "pub struct {}{lifetime} {{", class.rust_name);
        self.body.push_str(&fields);
        let _ = writeln!(self.body, "}}\n");

        let trait_name = self.serde("SerializeObject");
        let serializer = self.serde("Serializer");
        let result = self.serde("Result");
        let anonymous = if lifetime.is_empty() { "" } else { "<'_>" };
        let _ = writeln!(
            self.body,
            "impl {trait_name} for {}{anonymous} {{",
            class.rust_name
        );
        let _ = writeln!(
            self.body,
            "    fn serialize_fields(&self, serializer: &mut dyn {serializer}) -> {result}<()> {{"
        );
        self.body.push_str(&body);
        let _ = writeln!(self.body, "        Ok(())\n    }}\n}}\n");
        Ok(())
    }

    /// The field type of a member and how it is passed to the serializer.
    fn property_type(&mut self, member: &Member) -> Result<(String, Pass)> {
        if let Some(ty) = member.config.and_then(|c| c.property_type) {
            let pass = if ty.starts_with('&') {
                Pass::Reference
            } else {
                Pass::Value
            };
            return Ok((ty.to_owned(), pass));
        }
        if let Some(base) = &member.spec.base {
            // Declared as a subclass, written as a pointer to the hierarchy root.
            let path = self.data_path(base.trim_end_matches('*'))?;
            return Ok(if member.required() {
                (format!("&'a {path}"), Pass::Reference)
            } else {
                (format!("Option<&'a {path}>"), Pass::Value)
            });
        }
        Ok(match &member.ty {
            CppType::Pointer(inner) => {
                let inner = self.element_type(inner)?;
                if member.required() {
                    (format!("&'a {inner}"), Pass::Reference)
                } else {
                    (format!("Option<&'a {inner}>"), Pass::Value)
                }
            }
            CppType::String => ("&'a str".to_owned(), Pass::Reference),
            CppType::OptionalIdx => ("Option<u64>".to_owned(), Pass::Value),
            CppType::Named(name) if self.model.is_enum(name) => (self.data_path(name)?, Pass::Value),
            CppType::Named(name) => (format!("&'a {}", self.data_path(name)?), Pass::Reference),
            CppType::Vector(inner) | CppType::IndexVector(inner) => {
                (format!("&'a [{}]", self.element_type(inner)?), Pass::Reference)
            }
            CppType::Set(_)
            | CppType::Map(..)
            | CppType::CaseInsensitiveMap(_)
            | CppType::CaseInsensitiveSet => {
                (format!("&'a {}", self.element_type(&member.ty)?), Pass::Reference)
            }
            CppType::Pair(..) => (self.element_type(&member.ty)?, Pass::Value),
            scalar => (self.element_type(scalar)?, Pass::Value),
        })
    }

    /// The Rust type a container holds for a member type (pointers hold their value).
    fn element_type(&mut self, ty: &CppType) -> Result<String> {
        Ok(match ty {
            CppType::Bool => "bool".into(),
            CppType::U8 => "u8".into(),
            CppType::I8 => "i8".into(),
            CppType::U16 => "u16".into(),
            CppType::I16 => "i16".into(),
            CppType::U32 => "u32".into(),
            CppType::I32 => "i32".into(),
            CppType::U64 | CppType::OptionalIdx => "u64".into(),
            CppType::I64 => "i64".into(),
            CppType::F32 => "f32".into(),
            CppType::F64 => "f64".into(),
            CppType::I128 => "i128".into(),
            CppType::U128 => "u128".into(),
            CppType::String => "String".into(),
            CppType::Named(name) => self.data_path(name)?,
            CppType::Pointer(inner) => self.element_type(inner)?,
            CppType::Vector(inner) | CppType::IndexVector(inner) => {
                format!("Vec<{}>", self.element_type(inner)?)
            }
            CppType::Set(inner) => format!("std::collections::BTreeSet<{}>", self.element_type(inner)?),
            CppType::Map(key, value) => format!(
                "std::collections::BTreeMap<{}, {}>",
                self.element_type(key)?,
                self.element_type(value)?
            ),
            CppType::CaseInsensitiveMap(inner) => {
                format!("rustdb_common::CaseInsensitiveMap<{}>", self.element_type(inner)?)
            }
            CppType::CaseInsensitiveSet => "rustdb_common::CaseInsensitiveSet".into(),
            CppType::Pair(a, b) => format!("({}, {})", self.element_type(a)?, self.element_type(b)?),
        })
    }

    /// The full path of a named type: the `data_type` of a property-struct class, else
    /// the generated or external type. Paths are never imported, as property structs
    /// carry the names of the types they describe.
    fn data_path(&self, name: &str) -> Result<String> {
        if let Some(class) = self.model.classes.get(name).filter(|c| c.properties) {
            let data_type = class
                .config
                .data_type
                .ok_or_else(|| format!("{name} is held by another class; configure its data_type"))?;
            return Ok(if class.home.crate_name == self.home.crate_name {
                format!("crate::{data_type}")
            } else {
                format!("{}::{data_type}", class.home.crate_name)
            });
        }
        self.model.named_path(name, &self.home)
    }

    /// The statement writing one member.
    fn write_property_member(&mut self, member: &Member, pass: Pass) -> Result<String> {
        let (id, tag) = (member.spec.id, &member.spec.name);
        let field = format!("self.{}", member.rust_name);
        let value = match pass {
            Pass::Value => format!("&{field}"),
            Pass::Reference => field.clone(),
        };
        if let CppType::IndexVector(_) = member.ty {
            let index_vector = self.serde("IndexVector");
            return Ok(format!(
                "serializer.write_property({id}, {tag:?}, &{index_vector}({field}))?;"
            ));
        }
        if member.spec.base.is_some() {
            return Ok(if member.required() {
                format!("serializer.write_optional({id}, {tag:?}, false, {value})?;")
            } else {
                format!("serializer.write_property_with_default({id}, {tag:?}, {value})?;")
            });
        }
        Ok(match self.property_default(member)? {
            Some(DefaultExpr::NullValue) => {
                format!("serializer.write_optional({id}, {tag:?}, {field}.is_null(), {value})?;")
            }
            Some(DefaultExpr::InvalidIndex) => {
                let idx = self.serde("OptionalIdx");
                format!(
                    "serializer.write_property_with_explicit_default({id}, {tag:?}, &{idx}({field}), &{idx}(None))?;"
                )
            }
            Some(DefaultExpr::Expr(default)) => {
                let is_default = match (default.as_str(), pass) {
                    ("true", _) => field.clone(),
                    ("false", _) => format!("!{field}"),
                    ("*\"\"", _) => format!("{field}.is_empty()"),
                    (_, Pass::Value) => format!("{field} == {default}"),
                    (_, Pass::Reference) => format!("*{field} == {default}"),
                };
                format!("serializer.write_optional({id}, {tag:?}, {is_default}, {value})?;")
            }
            None if has_default_by_default(&member.spec.cpp_type) => {
                if member.required() {
                    format!("serializer.write_optional({id}, {tag:?}, false, {value})?;")
                } else {
                    format!("serializer.write_property_with_default({id}, {tag:?}, {value})?;")
                }
            }
            None if member.ty == CppType::OptionalIdx => {
                let idx = self.serde("OptionalIdx");
                format!("serializer.write_property({id}, {tag:?}, &{idx}({field}))?;")
            }
            None => format!("serializer.write_property({id}, {tag:?}, {value})?;"),
        })
    }

    /// The spec's explicit default of a member, as a value comparable with the field.
    fn property_default(&self, member: &Member) -> Result<Option<DefaultExpr>> {
        let Some(default) = &member.spec.default else {
            return Ok(None);
        };
        let text = match default {
            serde_json::Value::Bool(b) => return Ok(Some(DefaultExpr::Expr(b.to_string()))),
            serde_json::Value::Number(n) => return Ok(Some(DefaultExpr::Expr(n.to_string()))),
            serde_json::Value::String(s) => s.as_str(),
            other => return Err(format!("unsupported default {other}").into()),
        };
        let expr = match text {
            "true" | "false" => text.to_owned(),
            "optional_idx()" => return Ok(Some(DefaultExpr::InvalidIndex)),
            "Value()" => return Ok(Some(DefaultExpr::NullValue)),
            // Compared with the borrowed `str`.
            _ if text.starts_with('"') => format!("*{text}"),
            _ => match text.split_once("::") {
                Some(("LogicalType", constant)) => {
                    format!("{}::{constant}", self.data_path("LogicalType")?)
                }
                Some((enum_name, value)) if self.model.is_enum(enum_name) => {
                    if !self.model.enums[enum_name]
                        .members
                        .iter()
                        .any(|m| m.name == value)
                    {
                        return Err(format!("default {text}: no such member").into());
                    }
                    format!(
                        "{}::{}",
                        self.data_path(enum_name)?,
                        variant_name(enum_name, value)
                    )
                }
                _ => {
                    return Err(format!("{}: unsupported default '{text}'", member.spec.name).into());
                }
            },
        };
        Ok(Some(DefaultExpr::Expr(expr)))
    }
}
