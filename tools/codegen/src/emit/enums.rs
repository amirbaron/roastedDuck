//! C++ enums: the Rust enum, its `EnumUtil` names and its (de)serialization.

use std::fmt::Write as _;

use super::Emitter;
use crate::Result;
use crate::config::ENUM_VARIANT_NAMES;
use crate::cpp_enums::EnumDef;
use crate::names::{camel_case, rust_type_name};

impl Emitter<'_> {
    pub fn emit_enum(&mut self, name: &str, def: &EnumDef) -> Result<()> {
        let rust = rust_type_name(name);
        let variant = |member: &str| variant_name(name, member);
        let (repr, write, read) = match def.underlying.as_str() {
            "uint8_t" => ("u8", "write_u8", "read_u8"),
            "int8_t" => ("i8", "write_i8", "read_i8"),
            "uint16_t" => ("u16", "write_u16", "read_u16"),
            "int16_t" => ("i16", "write_i16", "read_i16"),
            "uint32_t" => ("u32", "write_u32", "read_u32"),
            "int32_t" | "int" => ("i32", "write_i32", "read_i32"),
            "uint64_t" | "idx_t" => ("u64", "write_u64", "read_u64"),
            "int64_t" => ("i64", "write_i64", "read_i64"),
            other => return Err(format!("{name}: unsupported underlying type {other}").into()),
        };
        let serialize = self.serde("Serialize");
        let deserialize = self.serde("Deserialize");
        let serializer = self.serde("Serializer");
        let deserializer = self.serde("Deserializer");
        let result = self.serde("Result");
        let write_enum = self.serde("write_enum");
        let read_enum = self.serde("read_enum");

        let b = &mut self.body;
        let _ = writeln!(b, "/// DuckDB's `{name}` (`{}`).", def.header);
        let _ = writeln!(
            b,
            "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]"
        );
        let _ = writeln!(b, "#[repr({repr})]");
        let _ = writeln!(b, "pub enum {rust} {{");
        for member in &def.members {
            if let Some(doc) = &member.doc {
                let _ = writeln!(b, "    /// {}", escape_markdown(doc));
            }
            let _ = writeln!(b, "    {} = {},", variant(&member.name), member.value);
        }
        let _ = writeln!(b, "}}\n");

        let _ = writeln!(b, "impl {rust} {{");
        let _ = writeln!(b, "    /// Every member, in declaration order.");
        let _ = writeln!(b, "    pub const ALL: [Self; {}] = [", def.members.len());
        for member in &def.members {
            let _ = writeln!(b, "        Self::{},", variant(&member.name));
        }
        let _ = writeln!(b, "    ];\n");
        let _ = writeln!(b, "    /// The name `EnumUtil::ToChars` prints.");
        let _ = writeln!(b, "    pub const fn name(self) -> &'static str {{");
        let _ = writeln!(b, "        match self {{");
        for member in &def.members {
            let _ = writeln!(
                b,
                "            Self::{} => {:?},",
                variant(&member.name),
                member.strings[0]
            );
        }
        let _ = writeln!(b, "        }}\n    }}\n");
        let _ = writeln!(
            b,
            "    /// Parses any name `EnumUtil::FromString` accepts (ASCII case-insensitive)."
        );
        let _ = writeln!(b, "    pub fn from_name(name: &str) -> Option<Self> {{");
        let _ = writeln!(b, "        const NAMES: &[(&str, {rust})] = &[");
        for member in &def.members {
            for string in &member.strings {
                let _ = writeln!(b, "            ({string:?}, {rust}::{}),", variant(&member.name));
            }
        }
        let _ = writeln!(b, "        ];");
        let _ = writeln!(
            b,
            "        NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| *v)"
        );
        let _ = writeln!(b, "    }}\n");
        let _ = writeln!(b, "    /// The member with this numeric value.");
        let _ = writeln!(b, "    pub const fn from_repr(value: {repr}) -> Option<Self> {{");
        let _ = writeln!(b, "        match value {{");
        for member in &def.members {
            let _ = writeln!(
                b,
                "            {} => Some(Self::{}),",
                member.value,
                variant(&member.name)
            );
        }
        let _ = writeln!(b, "            _ => None,\n        }}\n    }}\n}}\n");

        let _ = writeln!(b, "impl {serialize} for {rust} {{");
        let _ = writeln!(
            b,
            "    fn serialize(&self, serializer: &mut dyn {serializer}) -> {result}<()> {{"
        );
        let _ = writeln!(
            b,
            "        {write_enum}(serializer, self.name(), |serializer| {{ serializer.{write}(*self as {repr}); }});"
        );
        let _ = writeln!(b, "        Ok(())\n    }}\n}}\n");
        let _ = writeln!(b, "impl {deserialize} for {rust} {{");
        let _ = writeln!(
            b,
            "    fn deserialize(deserializer: &mut dyn {deserializer}) -> {result}<Self> {{"
        );
        let _ = writeln!(
            b,
            "        {read_enum}(deserializer, {name:?}, Self::from_name, |deserializer| Ok(Self::from_repr(deserializer.{read}()?)))"
        );
        let _ = writeln!(b, "    }}\n}}\n");
        Ok(())
    }
}

/// The Rust variant name of a C++ enum member.
pub(super) fn variant_name(enum_name: &str, member: &str) -> String {
    ENUM_VARIANT_NAMES
        .iter()
        .find(|(e, _)| *e == enum_name)
        .and_then(|(_, renames)| renames.iter().find(|(m, _)| *m == member))
        .map_or_else(|| camel_case(member), |(_, v)| (*v).to_owned())
}

/// C++ comment text as a doc comment: code-like words (`ValueType`, `x_y`) become code
/// spans, and markdown characters are escaped so they are not read as links or HTML.
fn escape_markdown(text: &str) -> String {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            let core = word.trim_matches(|c: char| matches!(c, '(' | ')' | ',' | '.' | ':' | ';'));
            let has_inner_upper = core
                .chars()
                .zip(core.chars().skip(1))
                .any(|(a, b)| a.is_ascii_lowercase() && b.is_ascii_uppercase());
            if !core.is_empty() && (has_inner_upper || core.contains('_') || core.contains("::")) {
                return word.replacen(core, &format!("`{core}`"), 1);
            }
            let mut escaped = String::with_capacity(word.len());
            for c in word.chars() {
                if matches!(c, '[' | ']' | '<' | '>' | '*' | '`') {
                    escaped.push('\\');
                }
                escaped.push(c);
            }
            escaped
        })
        .collect();
    words.join(" ")
}
