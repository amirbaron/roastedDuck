//! Arrow extension types (DuckDB's `common/arrow/arrow_type_extension.cpp` and the
//! `ArrowTypeExtensionData` of `arrow_duck_schema.hpp`): DuckDB types exported under an
//! extension name, and imported back from it.
//!
//! DuckDB keeps the registered extensions in its `DBConfig`; the set is the one
//! `ArrowTypeExtensionSet::Initialize` registers, as no roastedduck extension adds any, so it
//! is built once and shared.

use std::sync::{Arc, OnceLock};

use roastedduck_common::{Error, Result};
use roastedduck_types::geometry::{Crs, CrsType};
use roastedduck_types::{LogicalType, StringT, TypeInfo, Vector};

use crate::client_properties::{ArrowFormatVersion, ArrowOffsetSize, ClientProperties};
use crate::converter::{SchemaField, binary_format};
use crate::duck_schema::{ArrowType, ArrowTypeInfo, ArrowVariableSizeType};
use crate::schema_metadata::ArrowSchemaMetadata;

/// What identifies an extension type in Arrow metadata (DuckDB's
/// `ArrowExtensionMetadata`): two extensions are the same when their names (and, for
/// `arrow.opaque`, vendor and type names) are, whatever their format.
#[derive(Clone, Debug, Default)]
pub struct ArrowExtensionMetadata {
    /// The extension name (e.g. `arrow.uuid` or `arrow.opaque`).
    extension_name: String,
    /// For `arrow.opaque`: the system that produced the type (e.g. `DuckDB`).
    vendor_name: String,
    /// For `arrow.opaque`: the vendor's name of the type (e.g. `hugeint`).
    type_name: String,
    /// The Arrow storage format (e.g. `w:16`).
    arrow_format: String,
}

impl PartialEq for ArrowExtensionMetadata {
    fn eq(&self, other: &Self) -> bool {
        self.extension_name == other.extension_name
            && self.type_name == other.type_name
            && self.vendor_name == other.vendor_name
    }
}

impl ArrowExtensionMetadata {
    /// The extension name of vendor types (`ARROW_EXTENSION_NON_CANONICAL`).
    pub const ARROW_EXTENSION_NON_CANONICAL: &str = "arrow.opaque";

    pub fn new(extension_name: &str, vendor_name: &str, type_name: &str, arrow_format: &str) -> Self {
        Self {
            extension_name: extension_name.to_owned(),
            vendor_name: vendor_name.to_owned(),
            type_name: type_name.to_owned(),
            arrow_format: arrow_format.to_owned(),
        }
    }

    pub fn extension_name(&self) -> &str {
        &self.extension_name
    }

    pub fn vendor_name(&self) -> &str {
        &self.vendor_name
    }

    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// Whether this is a canonical Arrow extension rather than a vendor type
    /// (`IsCanonical`).
    pub fn is_canonical(&self) -> bool {
        self.vendor_name.is_empty()
    }
}

/// Converts a vector between an extension's DuckDB type and its internal (storage) type:
/// `count` rows of `source` into `result` (DuckDB's `cast_arrow_duck_t` and
/// `cast_duck_arrow_t`).
pub type ArrowExtensionCast = fn(source: &Vector, result: &mut Vector, count: usize) -> Result<()>;

/// How an extension type maps onto DuckDB vectors (DuckDB's `ArrowTypeExtensionData`).
#[derive(Debug)]
pub struct ArrowTypeExtensionData {
    /// The DuckDB type (e.g. `UHUGEINT`).
    duckdb_type: LogicalType,
    /// The type of the vectors holding the Arrow data, when it differs from the DuckDB
    /// type a conversion turns it into.
    internal_type: LogicalType,
    /// The conversion of imported data, if the types differ.
    pub arrow_to_duckdb: Option<ArrowExtensionCast>,
    /// The conversion of exported data, if the types differ.
    pub duckdb_to_arrow: Option<ArrowExtensionCast>,
}

impl ArrowTypeExtensionData {
    /// An extension whose data DuckDB holds in its own type.
    fn new(duckdb_type: LogicalType) -> Self {
        Self {
            internal_type: duckdb_type.clone(),
            duckdb_type,
            arrow_to_duckdb: None,
            duckdb_to_arrow: None,
        }
    }

    /// An extension stored as `internal_type`, converted with the given functions.
    fn with_conversions(
        duckdb_type: LogicalType,
        internal_type: LogicalType,
        arrow_to_duckdb: ArrowExtensionCast,
        duckdb_to_arrow: ArrowExtensionCast,
    ) -> Self {
        Self {
            duckdb_type,
            internal_type,
            arrow_to_duckdb: Some(arrow_to_duckdb),
            duckdb_to_arrow: Some(duckdb_to_arrow),
        }
    }

    /// `GetInternalType`
    pub fn internal_type(&self) -> &LogicalType {
        &self.internal_type
    }
}

/// Fills in the schema of an exported extension type (`populate_arrow_schema_t`).
type PopulateArrowSchema =
    fn(&mut SchemaField, &LogicalType, &ClientProperties, &ArrowTypeExtension) -> Result<()>;

/// The type of an imported field of an extension type, from its format and metadata
/// (`get_type_t`).
type GetArrowType = fn(format: &str, metadata: &ArrowSchemaMetadata) -> Result<ArrowType>;

/// A registered extension type (DuckDB's `ArrowTypeExtension`).
#[derive(Debug)]
pub struct ArrowTypeExtension {
    extension_metadata: ArrowExtensionMetadata,
    /// How the schema is written, when the format depends on the settings.
    populate_arrow_schema: Option<PopulateArrowSchema>,
    /// How the type is read, when the format determines it.
    get_type: Option<GetArrowType>,
    type_extension: Arc<ArrowTypeExtensionData>,
}

impl ArrowTypeExtension {
    /// A canonical extension with a fixed format.
    fn canonical(extension_name: &str, arrow_format: &str, data: ArrowTypeExtensionData) -> Self {
        Self {
            extension_metadata: ArrowExtensionMetadata::new(extension_name, "", "", arrow_format),
            populate_arrow_schema: None,
            get_type: None,
            type_extension: Arc::new(data),
        }
    }

    /// A vendor (`arrow.opaque`) extension with a fixed format.
    fn vendor(vendor_name: &str, type_name: &str, arrow_format: &str, data: ArrowTypeExtensionData) -> Self {
        Self {
            extension_metadata: ArrowExtensionMetadata::new(
                ArrowExtensionMetadata::ARROW_EXTENSION_NON_CANONICAL,
                vendor_name,
                type_name,
                arrow_format,
            ),
            populate_arrow_schema: None,
            get_type: None,
            type_extension: Arc::new(data),
        }
    }

    /// An extension whose format depends on the settings, written and read by callbacks.
    fn with_callbacks(
        extension_metadata: ArrowExtensionMetadata,
        populate_arrow_schema: PopulateArrowSchema,
        get_type: GetArrowType,
        data: ArrowTypeExtensionData,
    ) -> Self {
        Self {
            extension_metadata,
            populate_arrow_schema: Some(populate_arrow_schema),
            get_type: Some(get_type),
            type_extension: Arc::new(data),
        }
    }

    /// `GetInfo`
    pub fn info(&self) -> &ArrowExtensionMetadata {
        &self.extension_metadata
    }

    /// `GetTypeExtension`
    pub fn type_extension(&self) -> &Arc<ArrowTypeExtensionData> {
        &self.type_extension
    }

    /// The type of an imported field (`GetType`).
    pub fn get_type(&self, format: &str, metadata: &ArrowSchemaMetadata) -> Result<ArrowType> {
        match self.get_type {
            Some(get_type) => get_type(format, metadata),
            None => Ok(ArrowType::new(self.type_extension.duckdb_type.clone(), None)),
        }
    }

    /// Writes the schema of an exported field of this type (`PopulateArrowSchema`).
    pub(crate) fn populate_arrow_schema(
        &self,
        child: &mut SchemaField,
        duckdb_type: &LogicalType,
        options: &ClientProperties,
    ) -> Result<()> {
        if let Some(populate) = self.populate_arrow_schema {
            return populate(child, duckdb_type, options, self);
        }
        child.set_owned_format(&self.extension_metadata.arrow_format);
        let metadata = if self.extension_metadata.is_canonical() {
            ArrowSchemaMetadata::arrow_canonical_type(&self.extension_metadata.extension_name)
        } else {
            ArrowSchemaMetadata::non_canonical_type(
                &self.extension_metadata.type_name,
                &self.extension_metadata.vendor_name,
            )
        };
        child.set_metadata(metadata.serialize());
        Ok(())
    }
}

/// The registered extension types (DuckDB's `ArrowTypeExtensionSet`).
#[derive(Debug)]
pub struct ArrowTypeExtensionSet {
    type_extensions: Vec<ArrowTypeExtension>,
}

impl ArrowTypeExtensionSet {
    /// The extension DuckDB types export as, matched by type id and alias
    /// (`DBConfig::HasArrowExtension` + `GetArrowExtension` of a type; DuckDB's second
    /// lookup, by alias alone, finds extensions registered without a type, of which there
    /// are none).
    pub fn for_type(&self, ty: &LogicalType) -> Option<&ArrowTypeExtension> {
        let alias = ty
            .alias()
            .map_or(b"".as_slice(), roastedduck_common::ByteString::as_bytes);
        self.type_extensions.iter().find(|extension| {
            let duckdb_type = &extension.type_extension.duckdb_type;
            duckdb_type.id() == ty.id()
                && duckdb_type
                    .alias()
                    .map_or(b"".as_slice(), roastedduck_common::ByteString::as_bytes)
                    == alias
        })
    }

    /// The extension an imported field names (`DBConfig::HasArrowExtension` +
    /// `GetArrowExtension` of metadata).
    pub fn for_metadata(&self, info: &ArrowExtensionMetadata) -> Option<&ArrowTypeExtension> {
        self.type_extensions
            .iter()
            .find(|extension| &extension.extension_metadata == info)
    }
}

/// The built-in extension types (`ArrowTypeExtensionSet::Initialize`).
pub fn arrow_type_extensions() -> &'static ArrowTypeExtensionSet {
    static EXTENSIONS: OnceLock<ArrowTypeExtensionSet> = OnceLock::new();
    EXTENSIONS.get_or_init(|| {
        let duckdb = |type_name: &str| {
            ArrowExtensionMetadata::new(
                ArrowExtensionMetadata::ARROW_EXTENSION_NON_CANONICAL,
                "DuckDB",
                type_name,
                "",
            )
        };
        ArrowTypeExtensionSet {
            type_extensions: vec![
                // Types that are 1:1.
                ArrowTypeExtension::canonical(
                    "arrow.uuid",
                    "w:16",
                    ArrowTypeExtensionData::new(LogicalType::UUID),
                ),
                ArrowTypeExtension::canonical(
                    "arrow.bool8",
                    "c",
                    ArrowTypeExtensionData::with_conversions(
                        LogicalType::BOOLEAN,
                        LogicalType::TINYINT,
                        bool8_arrow_to_duck,
                        bool8_duck_to_arrow,
                    ),
                ),
                ArrowTypeExtension::vendor(
                    "DuckDB",
                    "hugeint",
                    "w:16",
                    ArrowTypeExtensionData::new(LogicalType::HUGEINT),
                ),
                ArrowTypeExtension::vendor(
                    "DuckDB",
                    "uhugeint",
                    "w:16",
                    ArrowTypeExtensionData::new(LogicalType::UHUGEINT),
                ),
                ArrowTypeExtension::vendor(
                    "DuckDB",
                    "time_tz",
                    "w:8",
                    ArrowTypeExtensionData::new(LogicalType::TIME_TZ),
                ),
                ArrowTypeExtension::with_callbacks(
                    ArrowExtensionMetadata::new("geoarrow.wkb", "", "", ""),
                    geometry_populate_schema,
                    geometry_get_type,
                    ArrowTypeExtensionData::with_conversions(
                        LogicalType::GEOMETRY,
                        LogicalType::BLOB,
                        geometry_arrow_to_duck,
                        geometry_duck_to_arrow,
                    ),
                ),
                // Types that are 1:n.
                ArrowTypeExtension::with_callbacks(
                    ArrowExtensionMetadata::new("arrow.json", "", "", ""),
                    json_populate_schema,
                    json_get_type,
                    ArrowTypeExtensionData::new(json_type()),
                ),
                ArrowTypeExtension::with_callbacks(
                    duckdb("bit"),
                    binary_populate_schema,
                    bit_get_type,
                    ArrowTypeExtensionData::new(LogicalType::BIT),
                ),
                ArrowTypeExtension::with_callbacks(
                    duckdb("bignum"),
                    binary_populate_schema,
                    bignum_get_type,
                    ArrowTypeExtensionData::new(LogicalType::BIGNUM),
                ),
            ],
        }
    })
}

/// The JSON type (`LogicalType::JSON()`): VARCHAR aliased as `JSON`.
pub(crate) fn json_type() -> LogicalType {
    LogicalType::VARCHAR.with_alias("JSON")
}

/// The string layout a format names, for the string-backed extensions.
fn string_info(format: &str, binary: bool) -> Option<ArrowTypeInfo> {
    let size_type = match (format, binary) {
        ("u", false) | ("z", true) => ArrowVariableSizeType::Normal,
        ("U", false) | ("Z", true) => ArrowVariableSizeType::SuperSize,
        ("vu", false) | ("vz", true) => ArrowVariableSizeType::View,
        _ => return None,
    };
    Some(ArrowTypeInfo::String {
        size_type,
        fixed_size: 0,
    })
}

/// `ArrowJson::GetType`
fn json_get_type(format: &str, _: &ArrowSchemaMetadata) -> Result<ArrowType> {
    let info = string_info(format, false).ok_or_else(|| {
        Error::invalid_input(format!(
            "Arrow extension type \"{format}\" not supported for arrow.json"
        ))
    })?;
    Ok(ArrowType::new(json_type(), Some(info)))
}

/// `ArrowJson::PopulateSchema`: a string, as a view only when string views are produced.
#[allow(clippy::unnecessary_wraps, reason = "the signature of extension callbacks")]
fn json_populate_schema(
    schema: &mut SchemaField,
    _: &LogicalType,
    options: &ClientProperties,
    extension: &ArrowTypeExtension,
) -> Result<()> {
    let metadata = ArrowSchemaMetadata::arrow_canonical_type(extension.info().extension_name());
    schema.set_metadata(metadata.serialize());
    // A view only when string views are produced, as the appender writes them then.
    schema.set_format(
        if options.produce_arrow_string_view && options.arrow_output_version >= ArrowFormatVersion::V1_4 {
            c"vu"
        } else if options.arrow_offset_size == ArrowOffsetSize::Large {
            c"U"
        } else {
            c"u"
        },
    );
    Ok(())
}

/// `ArrowBit::PopulateSchema` / `ArrowBignum::PopulateSchema`: a vendor binary type.
#[allow(clippy::unnecessary_wraps, reason = "the signature of extension callbacks")]
fn binary_populate_schema(
    schema: &mut SchemaField,
    _: &LogicalType,
    options: &ClientProperties,
    extension: &ArrowTypeExtension,
) -> Result<()> {
    let metadata =
        ArrowSchemaMetadata::non_canonical_type(extension.info().type_name(), extension.info().vendor_name());
    schema.set_metadata(metadata.serialize());
    schema.set_format(binary_format(options));
    Ok(())
}

/// `ArrowBit::GetType`
fn bit_get_type(format: &str, _: &ArrowSchemaMetadata) -> Result<ArrowType> {
    let info = string_info(format, true).ok_or_else(|| {
        Error::invalid_input(format!(
            "Arrow extension type \"{format}\" not supported for BIT type"
        ))
    })?;
    Ok(ArrowType::new(LogicalType::BIT, Some(info)))
}

/// `ArrowBignum::GetType`
fn bignum_get_type(format: &str, _: &ArrowSchemaMetadata) -> Result<ArrowType> {
    let info = string_info(format, true).ok_or_else(|| {
        Error::invalid_input(format!(
            "Arrow extension type \"{format}\" not supported for Bignum"
        ))
    })?;
    Ok(ArrowType::new(LogicalType::BIGNUM, Some(info)))
}

/// `ArrowBool8::ArrowToDuck`: one byte per value, any non-zero byte is true.
#[allow(clippy::unnecessary_wraps, reason = "the signature of extension callbacks")]
fn bool8_arrow_to_duck(source: &Vector, result: &mut Vector, count: usize) -> Result<()> {
    let source = source.values::<i8>();
    let target = result.values_mut::<bool>();
    for (target, &value) in target.iter_mut().zip(source).take(count) {
        *target = value != 0;
    }
    Ok(())
}

/// `ArrowBool8::DuckToArrow`: the rows of any vector representation as bytes.
#[allow(clippy::unnecessary_wraps, reason = "the signature of extension callbacks")]
fn bool8_duck_to_arrow(source: &Vector, result: &mut Vector, count: usize) -> Result<()> {
    let source = source.flattened(count);
    let values = source.values::<bool>();
    for (row, &value) in values.iter().enumerate().take(count) {
        if source.validity().is_valid(row) {
            result.values_mut::<i8>()[row] = i8::from(value);
        } else {
            result.set_null(row);
        }
    }
    Ok(())
}

/// `ArrowGeometry::GetType`: WKB, with the CRS of the `GeoArrow` metadata if DuckDB knows
/// it; only planar edges are supported.
fn geometry_get_type(format: &str, metadata: &ArrowSchemaMetadata) -> Result<ArrowType> {
    let extension_metadata = metadata.option(ArrowSchemaMetadata::ARROW_METADATA_KEY);
    let mut crs = None;
    if !extension_metadata.is_empty() {
        let document: serde_json::Value = serde_json::from_str(&extension_metadata)
            .map_err(|_| Error::serialization("Invalid JSON in GeoArrow metadata"))?;
        let serde_json::Value::Object(object) = document else {
            return Err(Error::serialization(
                "Invalid GeoArrow metadata: not a JSON object",
            ));
        };
        if let Some(serde_json::Value::String(edges)) = object.get("edges")
            && edges != "planar"
        {
            return Err(Error::not_implemented("Can't import non-planar edges"));
        }
        crs = match object.get("crs") {
            Some(serde_json::Value::String(definition)) => {
                roastedduck_types::geometry::try_identify(definition)?
            }
            Some(definition @ serde_json::Value::Object(_)) => {
                roastedduck_types::geometry::try_identify(&definition.to_string())?
            }
            _ => None,
        };
    }
    let geo_type = match crs {
        Some(crs) => LogicalType::geometry_with_crs(crs.definition)?,
        None => LogicalType::GEOMETRY,
    };
    let info = string_info(format, true).ok_or_else(|| {
        Error::invalid_input(format!(
            "Arrow extension type \"{format}\" not supported for geoarrow.wkb"
        ))
    })?;
    Ok(ArrowType::new(geo_type, Some(info)))
}

/// The `GeoArrow` metadata of a CRS (`ArrowGeometry::WriteCRS`): in the first form DuckDB
/// can convert it to of PROJJSON, WKT2:2019, `AUTH:CODE` and SRID.
fn geometry_crs_json(crs: &Crs) -> Result<String> {
    let mut converted = None;
    for target in [
        CrsType::Projjson,
        CrsType::Wkt2_2019,
        CrsType::AuthCode,
        CrsType::Srid,
    ] {
        converted = roastedduck_types::geometry::try_convert(crs, target)?;
        if converted.is_some() {
            break;
        }
    }
    let crs = converted.as_ref().unwrap_or(crs);
    let quote = roastedduck_common::string_util::json_quote;
    let (crs_type, value) = match crs.crs_type {
        CrsType::Projjson => {
            let document: serde_json::Value = serde_json::from_str(&crs.definition)
                .map_err(|_| Error::serialization("Could not parse PROJJSON CRS for GeoArrow metadata"))?;
            ("projjson", document.to_string())
        }
        CrsType::AuthCode => ("authority_code", quote(&crs.definition)),
        CrsType::Srid => ("srid", quote(&crs.definition)),
        CrsType::Wkt2_2019 => ("wkt2:2019", quote(&crs.definition)),
        CrsType::Invalid => {
            return Err(Error::serialization(
                "Could not serialize CRS of type 0 for GeoArrow metadata",
            ));
        }
    };
    Ok(format!("{{\"crs_type\":{},\"crs\":{value}}}", quote(crs_type)))
}

/// `ArrowGeometry::PopulateSchema`: `geoarrow.wkb` with the type's CRS.
fn geometry_populate_schema(
    schema: &mut SchemaField,
    ty: &LogicalType,
    options: &ClientProperties,
    _: &ArrowTypeExtension,
) -> Result<()> {
    let mut metadata = ArrowSchemaMetadata::default();
    metadata.add_option(ArrowSchemaMetadata::ARROW_EXTENSION_NAME, "geoarrow.wkb");
    let json = match ty.info() {
        Some(TypeInfo::Geo(info)) => geometry_crs_json(&Crs::parse(&info.crs.definition)?)?,
        _ => "{}".to_owned(),
    };
    metadata.add_option(ArrowSchemaMetadata::ARROW_METADATA_KEY, &json);
    schema.set_metadata(metadata.serialize());
    schema.set_format(binary_format(options));
    Ok(())
}

/// `ArrowGeometry::ArrowToDuck` (`Geometry::FromBinary`, strict): WKB in any byte order
/// becomes DuckDB's little-endian geometry.
fn geometry_arrow_to_duck(source: &Vector, result: &mut Vector, count: usize) -> Result<()> {
    for row in 0..count {
        if !result.validity().is_valid(row) {
            continue;
        }
        let wkb =
            roastedduck_types::geometry::from_binary(source.string_bytes(row), true)?.unwrap_or_default();
        let geometry: StringT = result.add_string(&wkb);
        result.values_mut::<StringT>()[row] = geometry;
    }
    Ok(())
}

/// `ArrowGeometry::DuckToArrow` (`Geometry::ToBinary`): DuckDB stores geometries as WKB,
/// so the vector is the BLOB.
#[allow(clippy::unnecessary_wraps, reason = "the signature of extension callbacks")]
fn geometry_duck_to_arrow(source: &Vector, result: &mut Vector, _: usize) -> Result<()> {
    *result = source.reinterpret(LogicalType::BLOB)?;
    Ok(())
}
