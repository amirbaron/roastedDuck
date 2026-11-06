//! Writing and reading one member, following `generate_serialization.py`.

use super::{DefaultExpr, Emitter};
use crate::Result;
use crate::cpp_type::{CppType, has_default_by_default};
use crate::model::{Child, Class, Member, Shape};
use crate::names::camel_case;
use crate::spec::MemberStatus;

impl<'m> Emitter<'m> {
    pub(super) fn default_expr(&mut self, member: &Member) -> Result<Option<DefaultExpr>> {
        let Some(default) = &member.spec.default else {
            return Ok(None);
        };
        let text = match default {
            serde_json::Value::Bool(b) => return Ok(Some(DefaultExpr::Expr(b.to_string()))),
            serde_json::Value::Number(n) => return Ok(Some(DefaultExpr::Expr(n.to_string()))),
            serde_json::Value::String(s) => s.clone(),
            other => return Err(format!("unsupported default {other}").into()),
        };
        if text == "true" || text == "false" {
            return Ok(Some(DefaultExpr::Expr(text)));
        }
        if text == "optional_idx()" {
            return Ok(Some(DefaultExpr::InvalidIndex));
        }
        if text == "Value()" {
            return Ok(Some(DefaultExpr::NullValue));
        }
        if let Some((enum_name, value)) = text.split_once("::")
            && self.model.is_enum(enum_name)
        {
            let ty = self.named(enum_name)?;
            let def = &self.model.enums[enum_name];
            if !def.members.iter().any(|m| m.name == value) {
                return Err(format!("default {text}: no such member").into());
            }
            return Ok(Some(DefaultExpr::Expr(format!("{ty}::{}", camel_case(value)))));
        }
        if text.ends_with("()") {
            // `unordered_map<string, Value>()` etc.: the Rust type's default.
            let ty = self.member_type(member)?;
            let path = ty.split('<').next().unwrap_or(&ty);
            return Ok(Some(DefaultExpr::Expr(format!("{path}::default()"))));
        }
        if text.starts_with('"') {
            return Ok(Some(DefaultExpr::Expr(format!("String::from({text})"))));
        }
        Err(format!("{}: unsupported default '{text}'", member.spec.name).into())
    }

    /// Statement(s) writing one member.
    pub(super) fn write_member(&mut self, class: &Class, member: &Member) -> Result<String> {
        let serialize_with = member.config.and_then(|c| c.serialize_with);
        let conditional = member.version != 1;
        let write = match (member.status, conditional) {
            (MemberStatus::Existing, _) => true,
            (_, true) => {
                if serialize_with.is_none() {
                    return Err(format!(
                        "{}.{}: written for old versions only; needs serialize_with",
                        class.spec.class, member.spec.name
                    )
                    .into());
                }
                true
            }
            (_, false) => false,
        };
        if !write {
            return Ok(format!(
                "        // `{}` ({}) is no longer written.\n",
                member.spec.name, member.spec.id
            ));
        }
        let (id, tag) = (member.spec.id, &member.spec.name);
        let value = match serialize_with {
            Some(expr) => format!("({expr})"),
            None => format!("self.{}", member.rust_name),
        };
        let stmt = if member.spec.base.is_some() {
            self.write_base_member(class, member)?
        } else {
            match self.default_expr(member)? {
                Some(DefaultExpr::NullValue) => {
                    format!("serializer.write_optional({id}, {tag:?}, {value}.is_null(), &{value})?;")
                }
                Some(DefaultExpr::InvalidIndex) => {
                    let idx = self.serde("OptionalIdx");
                    format!(
                        "serializer.write_property_with_explicit_default({id}, {tag:?}, &{idx}({value}), &{idx}(None))?;"
                    )
                }
                Some(DefaultExpr::Expr(default)) => format!(
                    "serializer.write_property_with_explicit_default({id}, {tag:?}, &{value}, &{default})?;"
                ),
                None if has_default_by_default(&member.spec.cpp_type) => {
                    if member.required() {
                        format!("serializer.write_optional({id}, {tag:?}, false, &{value})?;")
                    } else {
                        format!("serializer.write_property_with_default({id}, {tag:?}, &{value})?;")
                    }
                }
                None if member.ty == CppType::OptionalIdx => {
                    let idx = self.serde("OptionalIdx");
                    format!("serializer.write_property({id}, {tag:?}, &{idx}({value}))?;")
                }
                None => format!("serializer.write_property({id}, {tag:?}, &{value})?;"),
            }
        };
        Ok(match (member.status, conditional) {
            (MemberStatus::Existing, false) => format!("        {stmt}\n"),
            (MemberStatus::Existing, true) => format!(
                "        if serializer.should_serialize({}) {{\n            {stmt}\n        }}\n",
                member.version
            ),
            _ => format!(
                "        if !serializer.should_serialize({}) {{\n            {stmt}\n        }}\n",
                member.version
            ),
        })
    }

    /// A member declared with a derived type but serialized as its hierarchy root
    /// (`"base"` in the spec): the root's discriminant precedes the derived members.
    pub(super) fn base_parts(&mut self, member: &Member) -> Result<(&'m Class, Child)> {
        let base = member.spec.base.as_deref().ok_or("no base")?;
        let base_class = self
            .model
            .classes
            .get(base.trim_end_matches('*'))
            .ok_or_else(|| format!("unknown base {base}"))?;
        if base_class.shape != Shape::Enum {
            return Err(format!("base {base} must be a hierarchy with no shared members").into());
        }
        let CppType::Pointer(inner) = &member.ty else {
            return Err("base member must be a pointer".into());
        };
        let CppType::Named(derived) = inner.as_ref() else {
            return Err("base member must point to a class".into());
        };
        let child = base_class
            .children
            .iter()
            .find(|c| c.class == *derived)
            .cloned()
            .ok_or_else(|| format!("{derived} is not a subclass of {base}"))?;
        Ok((base_class, child))
    }

    pub(super) fn write_base_member(&mut self, class: &Class, member: &Member) -> Result<String> {
        if !member.required() {
            return Err(format!(
                "{}.{}: base members must be required",
                class.spec.class, member.spec.name
            )
            .into());
        }
        let (base_class, child) = self.base_parts(member)?;
        let (_, index) = base_class.discriminant.clone().ok_or("no class type")?;
        let disc = &base_class.members[index];
        let value = self.discriminant_value(base_class, &child.enum_value)?;
        Ok(format!(
            "serializer.write_optional_with({}, {:?}, false, |serializer| {{\n\
             serializer.on_nullable_begin(true);\n\
             serializer.on_object_begin();\n\
             serializer.write_property({}, {:?}, &{value})?;\n\
             self.{}.serialize_fields(serializer)?;\n\
             serializer.on_object_end();\n\
             serializer.on_nullable_end();\n\
             Ok(())\n\
             }})?;",
            member.spec.id, member.spec.name, disc.spec.id, disc.spec.name, member.rust_name
        ))
    }

    /// A `let` statement reading one member into a local of the same name.
    pub(super) fn read_member(&mut self, class: &Class, member: &Member, mutable: bool) -> Result<String> {
        let (id, tag) = (member.spec.id, &member.spec.name);
        if member.status != MemberStatus::Existing {
            if member.status == MemberStatus::ReadOnly {
                return Err(format!(
                    "{}.{}: read-only members need a custom class",
                    class.spec.class, member.spec.name
                )
                .into());
            }
            let ty = self.rust_type(&member.ty, true, false)?;
            return Ok(format!(
                "        deserializer.read_deleted_property::<{ty}>({id}, {tag:?})?;\n"
            ));
        }
        let ty = self.member_type(member)?;
        let binding = if mutable {
            format!("mut {}", member.rust_name)
        } else {
            member.rust_name.clone()
        };
        let expr = if member.spec.base.is_some() {
            self.read_base_member(class, member)?
        } else {
            match self.default_expr(member)? {
                Some(DefaultExpr::NullValue) => {
                    let value = self.named("Value")?;
                    format!(
                        "deserializer.read_property_with_explicit_default({id}, {tag:?}, {value}::default())?"
                    )
                }
                Some(DefaultExpr::InvalidIndex) => {
                    let idx = self.serde("OptionalIdx");
                    format!("deserializer.read_property_with_explicit_default({id}, {tag:?}, {idx}(None))?.0")
                }
                Some(DefaultExpr::Expr(default)) => {
                    format!("deserializer.read_property_with_explicit_default({id}, {tag:?}, {default})?")
                }
                None if has_default_by_default(&member.spec.cpp_type) => {
                    if member.required() {
                        let required = self.serde("required");
                        format!(
                            "{required}(deserializer.read_property_with_default({id}, {tag:?})?, {:?}, {tag:?})?",
                            class.spec.class
                        )
                    } else {
                        format!("deserializer.read_property_with_default({id}, {tag:?})?")
                    }
                }
                None if member.ty == CppType::OptionalIdx => {
                    let idx = self.serde("OptionalIdx");
                    format!("deserializer.read_property::<{idx}>({id}, {tag:?})?.0")
                }
                None => format!("deserializer.read_property({id}, {tag:?})?"),
            }
        };
        Ok(format!("        let {binding}: {ty} = {expr};\n"))
    }

    pub(super) fn read_base_member(&mut self, class: &Class, member: &Member) -> Result<String> {
        let (base_class, child) = self.base_parts(member)?;
        let base_ty = self.named(&base_class.spec.class)?;
        let required = self.serde("required");
        let error = self.serde("Error");
        let child_class = self.child_class(&child)?;
        let value = if self.is_boxed(child_class) {
            "*value"
        } else {
            "value"
        };
        Ok(format!(
            "match *{required}::<Box<{base_ty}>>(deserializer.read_property_with_default({}, {:?})?, {:?}, {:?})? {{\n\
             {base_ty}::{}(value) => {value},\n\
             _ => return Err({error}::serialization(\"Failed to deserialize: property \\\"{}\\\" of {} has the wrong subclass\")),\n\
             }}",
            member.spec.id,
            member.spec.name,
            class.spec.class,
            member.spec.name,
            child.variant,
            member.spec.name,
            class.spec.class
        ))
    }
}
