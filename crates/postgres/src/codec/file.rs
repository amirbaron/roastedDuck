//! Binary COPY files have caller-supplied DuckDB types, without server OIDs.
use super::{Codec, Encoding as E, Options};
use roastedduck_common::{Error, Result};
use roastedduck_types::{LogicalType, LogicalTypeId as L};

impl Codec {
    /// A CREATE definition already contains the logical wire layout. It does
    /// not require database-specific metadata lookups (`CreateEmptyPostgresType`).
    pub fn for_created(ty: &LogicalType, options: Options) -> Result<Self> {
        fn wire(mut codec: Codec) -> Codec {
            codec.file_format = false;
            codec.oid = match codec.logical_type.id() {
                L::Boolean => 16,
                L::SmallInt => 21,
                L::Integer => 23,
                L::BigInt => 20,
                L::Float => 700,
                L::Double => 701,
                L::Varchar => 1043,
                L::Date => 1082,
                L::Time => 1083,
                L::TimeTz => 1266,
                L::Timestamp => 1114,
                L::TimestampTz => 1184,
                L::Interval => 1186,
                L::Uuid => 2950,
                _ => 0,
            };
            codec.encoding = match codec.encoding {
                E::Array { element, dimensions } => E::Array {
                    element: Box::new(wire(*element)),
                    dimensions,
                },
                E::Composite(children) => E::Composite(children.into_iter().map(wire).collect()),
                other => other,
            };
            codec
        }
        Ok(wire(Self::for_file(ty, options)?))
    }

    pub fn for_file(ty: &LogicalType, options: Options) -> Result<Self> {
        let encoding = match ty.id() {
            L::Boolean => E::Bool,
            L::SmallInt => E::I16,
            L::Integer => E::I32,
            L::UInteger => E::U32,
            L::BigInt => E::I64,
            L::Float => E::F32,
            L::Double => E::F64,
            L::Decimal => E::Numeric,
            L::Varchar | L::Blob => E::Bytes,
            L::Geometry => E::NativeGeometry,
            L::Date => E::Date,
            L::Time => E::Time,
            L::TimeTz => E::TimeTz,
            L::Timestamp => E::Timestamp,
            L::TimestampTz => E::TimestampTz,
            L::Interval => E::Interval,
            L::Uuid => E::Uuid,
            L::Enum => E::Enum,
            L::List => {
                let mut dimensions = 0;
                let mut child = ty;
                while child.id() == L::List {
                    dimensions += 1;
                    child = child
                        .child_type()
                        .ok_or_else(|| Error::internal("Postgres list without child type"))?;
                }
                E::Array {
                    element: Box::new(Self::for_file(child, options)?),
                    dimensions,
                }
            }
            L::Struct => E::Composite(
                ty.struct_children()
                    .unwrap_or_default()
                    .iter()
                    .map(|(_, child)| Self::for_file(child, options))
                    .collect::<Result<_>>()?,
            ),
            // Like upstream, unsupported types can still read NULLs or an empty file.
            _ => E::Unsupported,
        };
        Ok(Self {
            logical_type: ty.clone(),
            oid: 0,
            encoding,
            options,
            file_format: true,
        })
    }
}
