//! Spec classes: structs, hierarchy enums and their `SerializeObject` /
//! `DeserializeObject` impls.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::enums::variant_name;
use super::{Emitter, SERDE};
use crate::Result;
use crate::model::{Child, Class, Member, Shape};
use crate::spec::MemberStatus;

impl<'m> Emitter<'m> {
    pub fn emit_class(&mut self, class: &Class) -> Result<()> {
        if class.config.custom {
            // Hand-written; subclasses of a custom root are still generated as plain structs.
            return Ok(());
        }
        let is_child_unit = class.spec.base.is_some() && class.members.is_empty();
        if is_child_unit {
            return Ok(());
        }
        match class.shape {
            Shape::Plain => self.emit_struct(class)?,
            Shape::Enum => self.emit_enum_root(class)?,
            Shape::StructWithKind => self.emit_struct_with_kind(class)?,
        }
        self.emit_serde_macro(class);
        Ok(())
    }

    pub(super) fn emit_serde_macro(&mut self, class: &Class) {
        if !class.config.null_variants.is_empty() {
            return;
        }
        // Subclasses are only ever written through their hierarchy root.
        let macro_name = if class.pointer_held {
            "impl_pointee_serde"
        } else if class.spec.base.is_none() {
            "impl_object_serde"
        } else {
            return;
        };
        let _ = writeln!(self.body, "{SERDE}::{macro_name}!({});\n", class.rust_name);
    }

    pub(super) fn derives(&self, class: &Class) -> String {
        let mut derives = vec!["Clone", "Debug"];
        for derive in self.derives.iter().chain(class.config.derives) {
            if !derives.contains(derive) {
                derives.push(derive);
            }
        }
        format!("#[derive({})]", derives.join(", "))
    }

    pub(super) fn field_doc(member: &Member) -> String {
        format!("    /// `{}` (property {}).\n", member.spec.name, member.spec.id)
    }

    /// Fields of a struct: data members that are still part of the format, plus extra
    /// in-memory fields.
    pub(super) fn emit_fields(&mut self, class: &Class) -> Result<()> {
        let members: Vec<&Member> = class
            .data_members()
            .filter(|m| m.status == MemberStatus::Existing)
            .collect();
        for member in members {
            let ty = self.member_type(member)?;
            let doc = Self::field_doc(member);
            let _ = writeln!(self.body, "{doc}    pub {}: {ty},", member.rust_name);
        }
        Ok(())
    }

    pub(super) fn emit_struct(&mut self, class: &Class) -> Result<()> {
        let _ = writeln!(self.body, "/// DuckDB's `{}`.", class.spec.class);
        let _ = writeln!(self.body, "{}", self.derives(class));
        let _ = writeln!(self.body, "pub struct {} {{", class.rust_name);
        self.emit_fields(class)?;
        let _ = writeln!(self.body, "}}\n");
        self.emit_plain_serialize(class)?;
        self.emit_plain_deserialize(class)
    }

    pub(super) fn emit_plain_serialize(&mut self, class: &Class) -> Result<()> {
        let trait_name = self.serde("SerializeObject");
        let serializer = self.serde("Serializer");
        let result = self.serde("Result");
        let mut body = String::new();
        for member in &class.members {
            body.push_str(&self.write_member(class, member)?);
        }
        let _ = writeln!(self.body, "impl {trait_name} for {} {{", class.rust_name);
        let _ = writeln!(
            self.body,
            "    fn serialize_fields(&self, serializer: &mut dyn {serializer}) -> {result}<()> {{"
        );
        self.body.push_str(&body);
        let _ = writeln!(self.body, "        Ok(())\n    }}\n}}\n");
        Ok(())
    }

    pub(super) fn emit_plain_deserialize(&mut self, class: &Class) -> Result<()> {
        let trait_name = self.serde("DeserializeObject");
        let deserializer = self.serde("Deserializer");
        let result = self.serde("Result");
        let mut body = String::new();
        for member in &class.members {
            body.push_str(&self.read_member(class, member, false)?);
        }
        let fields = Self::field_list(class);
        let _ = writeln!(self.body, "impl {trait_name} for {} {{", class.rust_name);
        let _ = writeln!(
            self.body,
            "    fn deserialize_fields(deserializer: &mut dyn {deserializer}) -> {result}<Self> {{"
        );
        self.body.push_str(&body);
        let _ = writeln!(self.body, "        Ok(Self {{ {fields} }})\n    }}\n}}\n");
        Ok(())
    }

    pub(super) fn field_list(class: &Class) -> String {
        class
            .data_members()
            .filter(|m| m.status == MemberStatus::Existing)
            .map(|m| m.rust_name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub(super) fn child_class(&self, child: &Child) -> Result<&'m Class> {
        self.model
            .classes
            .get(&child.class)
            .ok_or_else(|| format!("unknown subclass {}", child.class).into())
    }

    /// The `class_type` enum and its variant path for `value`.
    pub(super) fn discriminant_value(&mut self, class: &Class, value: &str) -> Result<String> {
        let (enum_name, _) = class.discriminant.as_ref().ok_or("not a hierarchy root")?;
        let def = self
            .model
            .enums
            .get(enum_name)
            .ok_or_else(|| format!("unknown enum {enum_name}"))?;
        if !def.members.iter().any(|m| m.name == value) {
            return Err(format!("{enum_name} has no member {value}").into());
        }
        let ty = self.named(enum_name)?;
        let variant = variant_name(enum_name, value);
        Ok(format!("{ty}::{variant}"))
    }

    /// Variant declarations and the discriminant method for a hierarchy root.
    pub(super) fn emit_kind_enum(&mut self, class: &Class, enum_name: &str, doc: &str) -> Result<()> {
        let (disc_enum, _) = class.discriminant.clone().ok_or("not a hierarchy root")?;
        let disc_ty = self.named(&disc_enum)?;
        let method = class.discriminant_method().ok_or("not a hierarchy root")?;
        let _ = writeln!(self.body, "{doc}");
        let derives = self.derives(class);
        let _ = writeln!(self.body, "{derives}");
        let _ = writeln!(self.body, "pub enum {enum_name} {{");
        let mut arms = Vec::new();
        for child in class.children.iter().filter(|c| !c.variant.is_empty()) {
            let child_class = self.child_class(child)?;
            let value = self.discriminant_value(class, &child.enum_value)?;
            let _ = writeln!(
                self.body,
                "    /// `{}` (`{}::{}`).",
                child_class.spec.class, disc_enum, child.enum_value
            );
            if child_class.members.is_empty() && !child_class.config.custom {
                let _ = writeln!(self.body, "    {},", child.variant);
                arms.push(format!("Self::{} => {value},", child.variant));
            } else {
                let ty = self.named(&child.class)?;
                let ty = if self.is_boxed(child_class) {
                    format!("Box<{ty}>")
                } else {
                    ty
                };
                let _ = writeln!(self.body, "    {}({ty}),", child.variant);
                arms.push(format!("Self::{}(_) => {value},", child.variant));
            }
        }
        let _ = writeln!(self.body, "}}\n");
        let _ = writeln!(self.body, "impl {enum_name} {{");
        let _ = writeln!(
            self.body,
            "    /// The `{disc_enum}` this subclass is serialized with."
        );
        let _ = writeln!(self.body, "    pub const fn {method}(&self) -> {disc_ty} {{");
        let _ = writeln!(self.body, "        match self {{");
        for arm in arms {
            let _ = writeln!(self.body, "            {arm}");
        }
        let _ = writeln!(self.body, "        }}\n    }}\n");
        Ok(())
    }

    /// `match self { ... }` writing the subclass members.
    pub(super) fn serialize_kind_match(&self, class: &Class) -> Result<String> {
        // Variants of the same subclass (several `class_type` values) share one arm.
        let mut patterns: Vec<(&str, Vec<String>)> = Vec::new();
        let mut units = Vec::new();
        for child in class.children.iter().filter(|c| !c.variant.is_empty()) {
            let child_class = self.child_class(child)?;
            if child_class.members.is_empty() && !child_class.config.custom {
                units.push(format!("Self::{}", child.variant));
            } else if let Some((_, group)) = patterns.iter_mut().find(|(class, _)| *class == child.class) {
                group.push(format!("Self::{}(value)", child.variant));
            } else {
                patterns.push((&child.class, vec![format!("Self::{}(value)", child.variant)]));
            }
        }
        let mut arms: Vec<String> = patterns
            .iter()
            .map(|(_, group)| format!("{} => value.serialize_fields(serializer),", group.join(" | ")))
            .collect();
        if !units.is_empty() {
            arms.push(format!("{} => Ok(()),", units.join(" | ")));
        }
        Ok(format!("match self {{\n{}\n}}", arms.join("\n")))
    }

    /// `match <discriminant> { ... }` reading the subclass, as an expression of `kind_path`.
    /// Values without a subclass are listed explicitly, so that new enum members fail to
    /// compile until handled.
    pub(super) fn deserialize_kind_match(&mut self, class: &Class, kind_path: &str) -> Result<String> {
        let (enum_name, _) = class.discriminant.clone().ok_or("not a hierarchy root")?;
        let local = class.discriminant_method().ok_or("not a hierarchy root")?;
        let mut arms = Vec::new();
        let mut covered = BTreeSet::new();
        for child in &class.children {
            let child_class = self.child_class(child)?;
            covered.insert(child.enum_value.clone());
            let value = self.discriminant_value(class, &child.enum_value)?;
            let expr = if let Some(arm) = child_class.config.deserialize_arm {
                arm.to_owned()
            } else if child_class.members.is_empty() && !child_class.config.custom {
                format!("{kind_path}::{}", child.variant)
            } else {
                let ty = self.named(&child.class)?;
                let read = format!("{ty}::deserialize_fields(deserializer)?");
                let read = if self.is_boxed(child_class) {
                    format!("Box::new({read})")
                } else {
                    read
                };
                format!("{kind_path}::{}({read})", child.variant)
            };
            arms.push(format!("{value} => {expr},"));
        }
        let uncovered: Vec<String> = self
            .model
            .enums
            .get(&enum_name)
            .map(|def| {
                def.members
                    .iter()
                    .filter(|m| !covered.contains(&m.name))
                    .map(|m| m.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        if !uncovered.is_empty() {
            let values = uncovered
                .iter()
                .map(|value| self.discriminant_value(class, value))
                .collect::<Result<Vec<_>>>()?;
            let error = self.serde("Error");
            arms.push(format!(
                "{} => return Err({error}::serialization(\"Unsupported type for deserialization of {}!\")),",
                values.join(" | "),
                class.spec.class
            ));
        }
        Ok(format!("match {local} {{\n{}\n}}", arms.join("\n")))
    }

    pub(super) fn emit_enum_root(&mut self, class: &Class) -> Result<()> {
        let doc = format!(
            "/// DuckDB's `{}` hierarchy: one variant per subclass.",
            class.spec.class
        );
        self.emit_kind_enum(class, &class.rust_name.clone(), &doc)?;
        let _ = writeln!(self.body, "}}\n");
        // Serialization: the discriminant, then the subclass members.
        let trait_name = self.serde("SerializeObject");
        let serializer = self.serde("Serializer");
        let result = self.serde("Result");
        let method = class.discriminant_method().ok_or("not a hierarchy root")?;
        let (_, index) = class.discriminant.clone().ok_or("not a hierarchy root")?;
        let disc = &class.members[index];
        let kind_match = self.serialize_kind_match(class)?;
        let _ = writeln!(self.body, "impl {trait_name} for {} {{", class.rust_name);
        let _ = writeln!(
            self.body,
            "    fn serialize_fields(&self, serializer: &mut dyn {serializer}) -> {result}<()> {{"
        );
        let _ = writeln!(
            self.body,
            "        serializer.write_property({}, {:?}, &self.{method}())?;",
            disc.spec.id, disc.spec.name
        );
        let _ = writeln!(self.body, "        {kind_match}\n    }}\n}}\n");

        let trait_name = self.serde("DeserializeObject");
        let deserializer = self.serde("Deserializer");
        let disc_ty = self.rust_type(&disc.ty, true, false)?;
        let kind_match = self.deserialize_kind_match(class, "Self")?;
        let _ = writeln!(self.body, "impl {trait_name} for {} {{", class.rust_name);
        let _ = writeln!(
            self.body,
            "    fn deserialize_fields(deserializer: &mut dyn {deserializer}) -> {result}<Self> {{"
        );
        let _ = writeln!(
            self.body,
            "        let {method}: {disc_ty} = deserializer.read_property({}, {:?})?;",
            disc.spec.id, disc.spec.name
        );
        let _ = writeln!(self.body, "        Ok({kind_match})\n    }}\n}}\n");
        Ok(())
    }

    pub(super) fn emit_struct_with_kind(&mut self, class: &Class) -> Result<()> {
        let kind_name = class.kind_name();
        let method = class.discriminant_method().ok_or("not a hierarchy root")?;
        let (disc_enum, index) = class.discriminant.clone().ok_or("not a hierarchy root")?;
        let disc = &class.members[index];
        let disc_ty = self.named(&disc_enum)?;

        let _ = writeln!(
            self.body,
            "/// DuckDB's `{}`: the members shared by all subclasses, with the subclass in `kind`.",
            class.spec.class
        );
        let _ = writeln!(self.body, "{}", self.derives(class));
        let _ = writeln!(self.body, "pub struct {} {{", class.rust_name);
        self.emit_fields(class)?;
        let _ = writeln!(
            self.body,
            "    /// The subclass, serialized as `{}` (property {}).\n    pub kind: {kind_name},",
            disc.spec.name, disc.spec.id
        );
        let _ = writeln!(self.body, "}}\n");

        let _ = writeln!(self.body, "impl {} {{", class.rust_name);
        let _ = writeln!(self.body, "    /// The `{disc_enum}` of this object's subclass.");
        let _ = writeln!(
            self.body,
            "    pub const fn {method}(&self) -> {disc_ty} {{\n        self.kind.{method}()\n    }}\n}}\n"
        );

        let doc = format!(
            "/// The subclasses of [`{}`], selected by its `{disc_enum}`.",
            class.rust_name
        );
        self.emit_kind_enum(class, &kind_name, &doc)?;
        let kind_match = self.serialize_kind_match(class)?;
        let serializer = self.serde("Serializer");
        let result = self.serde("Result");
        let _ = writeln!(
            self.body,
            "    fn serialize_fields(&self, serializer: &mut dyn {serializer}) -> {result}<()> {{\n        {kind_match}\n    }}\n}}\n"
        );

        // Serialization of the root: members in order, the discriminant taken from `kind`.
        let trait_name = self.serde("SerializeObject");
        let mut body = String::new();
        for (i, member) in class.members.iter().enumerate() {
            if i == index {
                let _ = writeln!(
                    body,
                    "        serializer.write_property({}, {:?}, &self.kind.{method}())?;",
                    member.spec.id, member.spec.name
                );
            } else {
                body.push_str(&self.write_member(class, member)?);
            }
        }
        let _ = writeln!(self.body, "impl {trait_name} for {} {{", class.rust_name);
        let _ = writeln!(
            self.body,
            "    fn serialize_fields(&self, serializer: &mut dyn {serializer}) -> {result}<()> {{"
        );
        self.body.push_str(&body);
        let _ = writeln!(
            self.body,
            "        self.kind.serialize_fields(serializer)\n    }}\n}}\n"
        );

        // Deserialization: members, then the subclass selected by the discriminant.
        let deserializer = self.serde("Deserializer");
        let nullable = !class.config.null_variants.is_empty();
        let mutable_locals: BTreeSet<String> = class
            .children
            .iter()
            .filter_map(|c| self.model.classes.get(&c.class))
            .filter_map(|c| c.config.deserialize_arm)
            .flat_map(|arm| {
                class
                    .members
                    .iter()
                    .filter(move |m| arm.contains(&format!("&mut {}", m.rust_name)))
                    .map(|m| m.rust_name.clone())
            })
            .collect();
        let mut body = String::new();
        for (i, member) in class.members.iter().enumerate() {
            if i == index {
                let _ = writeln!(
                    body,
                    "        let {method}: {disc_ty} = deserializer.read_property({}, {:?})?;",
                    member.spec.id, member.spec.name
                );
            } else {
                let read = self.read_member(class, member, mutable_locals.contains(&member.rust_name))?;
                body.push_str(&read);
            }
        }
        let kind_match = self.deserialize_kind_match(class, &kind_name)?;
        let fields = Self::field_list(class);
        let fields = if fields.is_empty() {
            "kind".to_owned()
        } else {
            format!("{fields}, kind")
        };
        if nullable {
            let _ = writeln!(self.body, "impl {} {{", class.rust_name);
            let _ = writeln!(
                self.body,
                "    /// Reads the properties written by `serialize_fields`; some subclasses read as no object."
            );
            let _ = writeln!(
                self.body,
                "    pub fn deserialize_fields(deserializer: &mut dyn {deserializer}) -> {result}<Option<Self>> {{"
            );
            self.body.push_str(&body);
            let _ = writeln!(self.body, "        let kind = {kind_match};");
            let _ = writeln!(self.body, "        Ok(Some(Self {{ {fields} }}))\n    }}\n}}\n");
        } else {
            let trait_name = self.serde("DeserializeObject");
            let _ = writeln!(self.body, "impl {trait_name} for {} {{", class.rust_name);
            let _ = writeln!(
                self.body,
                "    fn deserialize_fields(deserializer: &mut dyn {deserializer}) -> {result}<Self> {{"
            );
            self.body.push_str(&body);
            let _ = writeln!(self.body, "        let kind = {kind_match};");
            let _ = writeln!(self.body, "        Ok(Self {{ {fields} }})\n    }}\n}}\n");
        }
        Ok(())
    }
}
