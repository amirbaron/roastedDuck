//! The resolved generation model: spec classes with their configuration, Rust names,
//! types and hierarchy structure.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Result;
use crate::config::{self, ClassConfig, Content, EXTERNAL_TYPES, FieldConfig, TARGETS};
use crate::cpp_enums::EnumDef;
use crate::cpp_type::CppType;
use crate::names::{camel_case, rust_type_name, snake_case};
use crate::spec::{MemberStatus, SpecClass, SpecMember, VersionMap, read_spec_file};

/// Where a generated item lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    pub crate_name: &'static str,
    pub module: &'static str,
}

/// The Rust shape of a class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A struct (or a unit variant, for member-less subclasses).
    Plain,
    /// A hierarchy root whose only member selects the subclass: an enum of subclasses.
    Enum,
    /// A hierarchy root with shared members: a struct with a `kind` enum of subclasses.
    StructWithKind,
}

#[derive(Debug)]
pub struct Member {
    pub spec: SpecMember,
    pub config: Option<&'static FieldConfig>,
    pub ty: CppType,
    pub rust_name: String,
    pub status: MemberStatus,
    /// Serialization version the member was introduced in (1: always written).
    pub version: u64,
}

impl Member {
    pub fn required(&self) -> bool {
        self.config.is_some_and(|c| c.required)
    }
}

/// A subclass of a hierarchy root, selected by one `class_type` value.
#[derive(Debug, Clone)]
pub struct Child {
    /// The C++ enum member (`BETWEEN`).
    pub enum_value: String,
    pub class: String,
    pub variant: String,
}

#[derive(Debug)]
pub struct Class {
    pub spec: SpecClass,
    pub config: &'static ClassConfig,
    pub home: Home,
    /// Position in the spec files, for a stable output order.
    pub order: usize,
    pub rust_name: String,
    pub members: Vec<Member>,
    pub shape: Shape,
    /// Hierarchy roots: the enum type of the `class_type` member and its index.
    pub discriminant: Option<(String, usize)>,
    pub children: Vec<Child>,
    /// Held through a pointer somewhere (written as a nullable value).
    pub pointer_held: bool,
    /// Used by value somewhere (needs `Serialize`/`Deserialize` for whole objects).
    pub used_by_value: bool,
    /// Generated as a property struct for a hand-written type ([`Content::Properties`]).
    pub properties: bool,
}

impl Class {
    pub fn kind_name(&self) -> String {
        self.config
            .kind_name
            .map_or_else(|| format!("{}Kind", self.rust_name), str::to_owned)
    }

    /// Name of the method returning the `class_type` value (`expression_class`).
    pub fn discriminant_method(&self) -> Option<String> {
        self.discriminant.as_ref().map(|(ty, _)| snake_case(ty))
    }

    /// Members other than the `class_type` member.
    pub fn data_members(&self) -> impl Iterator<Item = &Member> {
        let skip = self.discriminant.as_ref().map(|(_, index)| *index);
        self.members
            .iter()
            .enumerate()
            .filter(move |(i, _)| Some(*i) != skip)
            .map(|(_, m)| m)
    }
}

#[derive(Debug)]
pub struct Model {
    pub classes: BTreeMap<String, Class>,
    pub enums: BTreeMap<String, EnumDef>,
    pub enum_homes: BTreeMap<String, Home>,
    pub versions: VersionMap,
}

impl Model {
    pub fn load(specs: &Path, enums: BTreeMap<String, EnumDef>) -> Result<Self> {
        let versions = VersionMap::read(&specs.join("version_map.json"))?;
        let mut classes = BTreeMap::new();
        let mut enum_homes = BTreeMap::new();
        for target in TARGETS {
            for module in target.modules {
                let home = Home {
                    crate_name: target.crate_name,
                    module: module.name,
                };
                match &module.content {
                    Content::Classes {
                        spec,
                        classes: selection,
                    }
                    | Content::Properties {
                        spec,
                        classes: selection,
                    } => {
                        let properties = matches!(module.content, Content::Properties { .. });
                        for class in read_spec_file(&specs.join(spec))? {
                            if selection.is_some_and(|s| !s.contains(&class.class.as_str())) {
                                continue;
                            }
                            let name = class.class.clone();
                            if classes.contains_key(&name) {
                                return Err(format!("{name} is generated twice").into());
                            }
                            let mut resolved = resolve_class(class, home.clone(), &versions)?;
                            resolved.order = classes.len();
                            resolved.properties = properties;
                            classes.insert(name, resolved);
                        }
                    }
                    Content::Enums(names) => {
                        for name in *names {
                            if !enums.contains_key(*name) {
                                return Err(format!("enum {name} missing from specs/enums.json").into());
                            }
                            enum_homes.insert((*name).to_owned(), home.clone());
                        }
                    }
                    Content::VersionMap => {}
                }
            }
        }
        let mut model = Self {
            classes,
            enums,
            enum_homes,
            versions,
        };
        model.link_hierarchies()?;
        model.compute_usage(specs)?;
        model.validate()?;
        Ok(model)
    }

    fn link_hierarchies(&mut self) -> Result<()> {
        // Subclasses in spec order, so variants follow DuckDB's declaration order.
        let mut names: Vec<String> = self.classes.keys().cloned().collect();
        names.sort_by_key(|name| self.classes[name].order);
        let mut links = Vec::new();
        for name in &names {
            let class = &self.classes[name];
            let Some(base) = &class.spec.base else { continue };
            let values = class
                .spec
                .enum_values
                .as_ref()
                .map(crate::spec::OneOrMany::to_vec)
                .unwrap_or_default();
            links.push((base.clone(), name.clone(), values));
        }
        for (base, child, values) in links {
            let child_config = config::class_config(&child);
            let Some(base_class) = self.classes.get_mut(&base) else {
                return Err(format!("{child}: unknown base class {base}").into());
            };
            if child_config.no_variant {
                // Deserialization-only: no variant, handled by `deserialize_arm`.
                for value in values {
                    base_class.children.push(Child {
                        enum_value: value,
                        class: child.clone(),
                        variant: String::new(),
                    });
                }
                continue;
            }
            for value in values {
                let variant = base_class
                    .config
                    .variant_names
                    .iter()
                    .find(|(v, _)| *v == value)
                    .map_or_else(
                        || {
                            let stripped = base_class
                                .config
                                .strip_suffix
                                .and_then(|suffix| value.strip_suffix(suffix))
                                .unwrap_or(&value);
                            camel_case(stripped)
                        },
                        |(_, name)| (*name).to_owned(),
                    );
                base_class.children.push(Child {
                    enum_value: value,
                    class: child.clone(),
                    variant,
                });
            }
        }
        Ok(())
    }

    /// Refuses classes whose spec relies on hand-written C++ that has no Rust counterpart
    /// configured, so that spec changes in a new DuckDB release cannot be silently missed.
    fn validate(&self) -> Result<()> {
        for class in self.classes.values().filter(|c| !c.config.custom) {
            let name = &class.spec.class;
            let spec = &class.spec;
            if class.properties {
                // Only written, by hand-written types: the C++ deserialization code and
                // computed properties have no counterpart.
                if spec.custom_implementation {
                    return Err(
                        format!("{name}: hand-written C++ serialization; configure a custom class").into(),
                    );
                }
                continue;
            }
            if spec.custom_implementation || spec.constructor_method.is_some() {
                return Err(
                    format!("{name}: hand-written C++ (de)serialization; configure a custom class").into(),
                );
            }
            if spec.finalize_deserialization.is_some() && class.config.finalize_ported_in.is_none() {
                return Err(format!("{name}: `finalize_deserialization` is not ported").into());
            }
            let unit_child = spec.base.is_some() && spec.members.is_empty();
            if spec.custom_switch_code.is_some() && class.config.deserialize_arm.is_none() && !unit_child {
                return Err(format!("{name}: `custom_switch_code` needs a `deserialize_arm`").into());
            }
            for member in &class.members {
                let computed = member
                    .spec
                    .serialize_property
                    .as_deref()
                    .is_some_and(|property| property.contains('('));
                if computed && member.config.and_then(|c| c.serialize_with).is_none() {
                    return Err(format!(
                        "{name}.{}: computed `serialize_property` needs `serialize_with`",
                        member.spec.name
                    )
                    .into());
                }
                if member.spec.deserialize_property.is_some() {
                    return Err(format!(
                        "{name}.{}: `deserialize_property` needs a custom class",
                        member.spec.name
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    /// Whether each class is held by pointer or by value, across all spec files (also
    /// those not generated yet, such as the logical plan's).
    fn compute_usage(&mut self, specs: &Path) -> Result<()> {
        let mut pointer_held = BTreeSet::new();
        let mut by_value = BTreeSet::new();
        let mut files: Vec<_> = std::fs::read_dir(specs)?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .filter(|path| {
                path.file_name()
                    .is_some_and(|n| n != "enums.json" && n != "version_map.json")
            })
            .collect();
        files.sort();
        for file in files {
            for class in read_spec_file(&file)? {
                // Members declared through a hierarchy root are written as that root.
                for member in class.members.iter().filter(|m| m.base.is_none()) {
                    if let Ok(ty) = CppType::parse(&member.cpp_type) {
                        collect_usage(&ty, false, &mut pointer_held, &mut by_value);
                    }
                }
            }
        }
        for (name, class) in &mut self.classes {
            class.pointer_held = pointer_held.contains(name);
            class.used_by_value = by_value.contains(name);
            if class.pointer_held && class.used_by_value {
                return Err(format!("{name} is used both by value and through a pointer").into());
            }
        }
        Ok(())
    }

    /// The Rust path of a named C++ type, as seen from `from`.
    pub fn named_path(&self, name: &str, from: &Home) -> Result<String> {
        let (crate_name, rust) = if let Some(class) = self.classes.get(name) {
            (class.home.crate_name, class.rust_name.clone())
        } else if let Some(home) = self.enum_homes.get(name) {
            (home.crate_name, rust_type_name(name))
        } else if let Some((_, krate, rust)) = EXTERNAL_TYPES.iter().find(|(cpp, _, _)| *cpp == name) {
            (*krate, (*rust).to_owned())
        } else {
            return Err(format!("unknown type {name} (not generated, not an enum, not external)").into());
        };
        Ok(if crate_name == from.crate_name {
            format!("crate::{rust}")
        } else {
            format!("{crate_name}::{rust}")
        })
    }

    /// Whether `name` is a generated enum.
    pub fn is_enum(&self, name: &str) -> bool {
        self.enum_homes.contains_key(name)
    }
}

fn collect_usage(
    ty: &CppType,
    under_pointer: bool,
    pointers: &mut BTreeSet<String>,
    values: &mut BTreeSet<String>,
) {
    match ty {
        CppType::Named(name) => {
            if under_pointer {
                pointers.insert(name.clone());
            } else {
                values.insert(name.clone());
            }
        }
        CppType::Pointer(inner) => collect_usage(inner, true, pointers, values),
        CppType::Vector(inner)
        | CppType::Set(inner)
        | CppType::CaseInsensitiveMap(inner)
        | CppType::IndexVector(inner) => {
            collect_usage(inner, false, pointers, values);
        }
        CppType::Map(a, b) | CppType::Pair(a, b) => {
            collect_usage(a, false, pointers, values);
            collect_usage(b, false, pointers, values);
        }
        _ => {}
    }
}

fn resolve_class(spec: SpecClass, home: Home, versions: &VersionMap) -> Result<Class> {
    let config = config::class_config(&spec.class);
    let rust_name = config
        .rust_name
        .map_or_else(|| rust_type_name(&spec.class), str::to_owned);
    let mut members = Vec::new();
    for member in &spec.members {
        let field_config = config.fields.iter().find(|f| f.member == member.name);
        let rust_name = field_config
            .and_then(|f| f.rename)
            .map_or_else(|| member.name.clone(), str::to_owned);
        let is_class_type =
            spec.class_type.as_deref() == Some(member.property.as_deref().unwrap_or(&member.name));
        let deleted = member.status()? != MemberStatus::Existing;
        if crate::names::is_keyword(&rust_name) && !is_class_type && !deleted {
            return Err(format!("{}.{}: Rust keyword, add a rename", spec.class, member.name).into());
        }
        members.push(Member {
            ty: CppType::parse(&member.cpp_type)?,
            status: member.status()?,
            version: versions.lookup(member.version.as_deref().unwrap_or("v0.10.2")),
            spec: member.clone(),
            config: field_config,
            rust_name,
        });
    }
    for field in config.fields {
        if !members.iter().any(|m| m.spec.name == field.member) {
            return Err(format!(
                "{}: configured member {} does not exist",
                spec.class, field.member
            )
            .into());
        }
    }
    let class_type = match &spec.class_type {
        Some(property) => {
            let index = members
                .iter()
                .position(|m| m.spec.property.as_deref().unwrap_or(&m.spec.name) == property)
                .ok_or_else(|| format!("{}: class_type {property} is not a member", spec.class))?;
            let CppType::Named(ty) = &members[index].ty else {
                return Err(format!("{}: class_type must be an enum", spec.class).into());
            };
            Some((ty.clone(), index))
        }
        None => None,
    };
    let shape = match &class_type {
        None => Shape::Plain,
        Some(_) if members.len() == 1 => Shape::Enum,
        Some(_) => Shape::StructWithKind,
    };
    Ok(Class {
        spec,
        config,
        home,
        order: 0,
        rust_name,
        members,
        shape,
        discriminant: class_type,
        children: Vec::new(),
        pointer_held: false,
        used_by_value: false,
        properties: false,
    })
}
