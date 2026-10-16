//! The Parquet metadata structures of `third_party/parquet/parquet.thrift` (the C++
//! `duckdb_parquet` namespace generated from it). Field ids, kinds and defaults follow the
//! IDL; Rust field names are the snake-case IDL names (`type` is `type_`).

use super::{Binary, thrift_enum, thrift_struct};

thrift_enum! {
    /// Physical types (`Type`).
    Type {
        BOOLEAN = 0,
        INT32 = 1,
        INT64 = 2,
        INT96 = 3,
        FLOAT = 4,
        DOUBLE = 5,
        BYTE_ARRAY = 6,
        FIXED_LEN_BYTE_ARRAY = 7,
    }
}

thrift_enum! {
    /// Legacy logical type annotations (`ConvertedType`).
    ConvertedType {
        UTF8 = 0,
        MAP = 1,
        MAP_KEY_VALUE = 2,
        LIST = 3,
        ENUM = 4,
        DECIMAL = 5,
        DATE = 6,
        TIME_MILLIS = 7,
        TIME_MICROS = 8,
        TIMESTAMP_MILLIS = 9,
        TIMESTAMP_MICROS = 10,
        UINT_8 = 11,
        UINT_16 = 12,
        UINT_32 = 13,
        UINT_64 = 14,
        INT_8 = 15,
        INT_16 = 16,
        INT_32 = 17,
        INT_64 = 18,
        JSON = 19,
        BSON = 20,
        INTERVAL = 21,
        PARQUET_NULL = 24,
    }
}

thrift_enum! {
    /// How often a field occurs (`FieldRepetitionType`).
    FieldRepetitionType {
        REQUIRED = 0,
        OPTIONAL = 1,
        REPEATED = 2,
    }
}

thrift_enum! {
    /// Value encodings (`Encoding`).
    Encoding {
        PLAIN = 0,
        PLAIN_DICTIONARY = 2,
        RLE = 3,
        BIT_PACKED = 4,
        DELTA_BINARY_PACKED = 5,
        DELTA_LENGTH_BYTE_ARRAY = 6,
        DELTA_BYTE_ARRAY = 7,
        RLE_DICTIONARY = 8,
        BYTE_STREAM_SPLIT = 9,
    }
}

thrift_enum! {
    /// Page compression codecs (`CompressionCodec`).
    CompressionCodec {
        UNCOMPRESSED = 0,
        SNAPPY = 1,
        GZIP = 2,
        LZO = 3,
        BROTLI = 4,
        LZ4 = 5,
        ZSTD = 6,
        LZ4_RAW = 7,
    }
}

thrift_enum! {
    /// Page kinds (`PageType`).
    PageType {
        DATA_PAGE = 0,
        INDEX_PAGE = 1,
        DICTIONARY_PAGE = 2,
        DATA_PAGE_V2 = 3,
    }
}

thrift_enum! {
    /// Ordering of the pages of a column index (`BoundaryOrder`).
    BoundaryOrder {
        UNORDERED = 0,
        ASCENDING = 1,
        DESCENDING = 2,
    }
}

thrift_enum! {
    /// Edge interpolation of geography values (`EdgeInterpolationAlgorithm`).
    EdgeInterpolationAlgorithm {
        SPHERICAL = 0,
        VINCENTY = 1,
        THOMAS = 2,
        ANDOYER = 3,
        KARNEY = 4,
    }
}

thrift_struct! {
    /// Sizes of the values of a column chunk (`SizeStatistics`).
    SizeStatistics {
        1: optional unencoded_byte_array_data_bytes: i64,
        2: optional repetition_level_histogram: Vec<i64>,
        3: optional definition_level_histogram: Vec<i64>,
    }
}

thrift_struct! {
    /// The bounding box of geometry values (`BoundingBox`).
    BoundingBox {
        1: required xmin: f64,
        2: required xmax: f64,
        3: required ymin: f64,
        4: required ymax: f64,
        5: optional zmin: f64,
        6: optional zmax: f64,
        7: optional mmin: f64,
        8: optional mmax: f64,
    }
}

thrift_struct! {
    /// Statistics of geometry values (`GeospatialStatistics`).
    GeospatialStatistics {
        1: optional bbox: BoundingBox,
        2: optional geospatial_types: Vec<i32>,
    }
}

thrift_struct! {
    /// Column chunk or page statistics (`Statistics`); `max`/`min` are the deprecated
    /// signed-order values.
    Statistics {
        1: optional max: Binary,
        2: optional min: Binary,
        3: optional null_count: i64,
        4: optional distinct_count: i64,
        5: optional max_value: Binary,
        6: optional min_value: Binary,
        7: optional is_max_value_exact: bool,
        8: optional is_min_value_exact: bool,
    }
}

thrift_struct! { /// `StringType`.
    StringType {} }
thrift_struct! { /// `UUIDType`.
    UuidType {} }
thrift_struct! { /// `MapType`.
    MapType {} }
thrift_struct! { /// `ListType`.
    ListType {} }
thrift_struct! { /// `EnumType`.
    EnumType {} }
thrift_struct! { /// `DateType`.
    DateType {} }
thrift_struct! { /// `Float16Type`.
    Float16Type {} }
thrift_struct! { /// `NullType`.
    NullType {} }
thrift_struct! { /// `JsonType`.
    JsonType {} }
thrift_struct! { /// `BsonType`.
    BsonType {} }
thrift_struct! { /// `MilliSeconds`.
    MilliSeconds {} }
thrift_struct! { /// `MicroSeconds`.
    MicroSeconds {} }
thrift_struct! { /// `NanoSeconds`.
    NanoSeconds {} }
thrift_struct! { /// `SplitBlockAlgorithm`.
    SplitBlockAlgorithm {} }
thrift_struct! { /// `XxHash`.
    XxHash {} }
thrift_struct! { /// `Uncompressed`.
    Uncompressed {} }
thrift_struct! { /// `TypeDefinedOrder`.
    TypeDefinedOrder {} }
thrift_struct! { /// `IndexPageHeader`.
    IndexPageHeader {} }
thrift_struct! { /// `EncryptionWithFooterKey`.
    EncryptionWithFooterKey {} }

thrift_struct! {
    /// `DecimalType`.
    DecimalType {
        1: required scale: i32,
        2: required precision: i32,
    }
}

thrift_struct! {
    /// The unit of a time or timestamp (union `TimeUnit`).
    TimeUnit {
        1: optional millis: MilliSeconds,
        2: optional micros: MicroSeconds,
        3: optional nanos: NanoSeconds,
    }
}

thrift_struct! {
    /// `TimestampType`.
    TimestampType {
        1: required is_adjusted_to_utc: bool,
        2: required unit: TimeUnit,
    }
}

thrift_struct! {
    /// `TimeType`.
    TimeType {
        1: required is_adjusted_to_utc: bool,
        2: required unit: TimeUnit,
    }
}

thrift_struct! {
    /// `IntType`.
    IntType {
        1: required bit_width: i8,
        2: required is_signed: bool,
    }
}

thrift_struct! {
    /// `VariantType`.
    VariantType {
        1: optional specification_version: i8,
    }
}

thrift_struct! {
    /// `GeometryType`.
    GeometryType {
        1: optional crs: String,
    }
}

thrift_struct! {
    /// `GeographyType`.
    GeographyType {
        1: optional crs: String,
        2: optional algorithm: EdgeInterpolationAlgorithm,
    }
}

thrift_struct! {
    /// The logical type annotation of a schema element (union `LogicalType`).
    LogicalType {
        1: optional string: StringType,
        2: optional map: MapType,
        3: optional list: ListType,
        4: optional enum_: EnumType,
        5: optional decimal: DecimalType,
        6: optional date: DateType,
        7: optional time: TimeType,
        8: optional timestamp: TimestampType,
        10: optional integer: IntType,
        11: optional unknown: NullType,
        12: optional json: JsonType,
        13: optional bson: BsonType,
        14: optional uuid: UuidType,
        15: optional float16: Float16Type,
        16: optional variant: VariantType,
        17: optional geometry: GeometryType,
        18: optional geography: GeographyType,
    }
}

thrift_struct! {
    /// A node of the flattened schema tree (`SchemaElement`).
    SchemaElement {
        1: optional type_: Type,
        2: optional type_length: i32,
        3: optional repetition_type: FieldRepetitionType,
        4: required name: String,
        5: optional num_children: i32,
        6: optional converted_type: ConvertedType,
        7: optional scale: i32,
        8: optional precision: i32,
        9: optional field_id: i32,
        10: optional logical_type: LogicalType,
    }
}

thrift_struct! {
    /// `DataPageHeader`.
    DataPageHeader {
        1: required num_values: i32,
        2: required encoding: Encoding,
        3: required definition_level_encoding: Encoding,
        4: required repetition_level_encoding: Encoding,
        5: optional statistics: Statistics,
    }
}

thrift_struct! {
    /// `DictionaryPageHeader`.
    DictionaryPageHeader {
        1: required num_values: i32,
        2: required encoding: Encoding,
        3: optional is_sorted: bool,
    }
}

thrift_struct! {
    /// `DataPageHeaderV2`; `is_compressed` is set (to true) by default, like in C++.
    DataPageHeaderV2 {
        1: required num_values: i32,
        2: required num_nulls: i32,
        3: required num_rows: i32,
        4: required encoding: Encoding,
        5: required definition_levels_byte_length: i32,
        6: required repetition_levels_byte_length: i32,
        7: optional is_compressed: bool = true,
        8: optional statistics: Statistics,
    }
}

thrift_struct! {
    /// Union `BloomFilterAlgorithm`.
    BloomFilterAlgorithm {
        1: optional block: SplitBlockAlgorithm,
    }
}

thrift_struct! {
    /// Union `BloomFilterHash`.
    BloomFilterHash {
        1: optional xxhash: XxHash,
    }
}

thrift_struct! {
    /// Union `BloomFilterCompression`.
    BloomFilterCompression {
        1: optional uncompressed: Uncompressed,
    }
}

thrift_struct! {
    /// `BloomFilterHeader`.
    BloomFilterHeader {
        1: required num_bytes: i32,
        2: required algorithm: BloomFilterAlgorithm,
        3: required hash: BloomFilterHash,
        4: required compression: BloomFilterCompression,
    }
}

thrift_struct! {
    /// The header in front of every page (`PageHeader`).
    PageHeader {
        1: required type_: PageType,
        2: required uncompressed_page_size: i32,
        3: required compressed_page_size: i32,
        4: optional crc: i32,
        5: optional data_page_header: DataPageHeader,
        6: optional index_page_header: IndexPageHeader,
        7: optional dictionary_page_header: DictionaryPageHeader,
        8: optional data_page_header_v2: DataPageHeaderV2,
    }
}

thrift_struct! {
    /// `KeyValue`.
    KeyValue {
        1: required key: Binary,
        2: optional value: Binary,
    }
}

thrift_struct! {
    /// `SortingColumn`.
    SortingColumn {
        1: required column_idx: i32,
        2: required descending: bool,
        3: required nulls_first: bool,
    }
}

thrift_struct! {
    /// `PageEncodingStats`.
    PageEncodingStats {
        1: required page_type: PageType,
        2: required encoding: Encoding,
        3: required count: i32,
    }
}

thrift_struct! {
    /// The metadata of a column chunk (`ColumnMetaData`).
    ColumnMetaData {
        1: required type_: Type,
        2: required encodings: Vec<Encoding>,
        3: required path_in_schema: Vec<String>,
        4: required codec: CompressionCodec,
        5: required num_values: i64,
        6: required total_uncompressed_size: i64,
        7: required total_compressed_size: i64,
        8: optional key_value_metadata: Vec<KeyValue>,
        9: required data_page_offset: i64,
        10: optional index_page_offset: i64,
        11: optional dictionary_page_offset: i64,
        12: optional statistics: Statistics,
        13: optional encoding_stats: Vec<PageEncodingStats>,
        14: optional bloom_filter_offset: i64,
        15: optional bloom_filter_length: i32,
        16: optional size_statistics: SizeStatistics,
        17: optional geospatial_statistics: GeospatialStatistics,
    }
}

thrift_struct! {
    /// `EncryptionWithColumnKey`.
    EncryptionWithColumnKey {
        1: required path_in_schema: Vec<String>,
        2: optional key_metadata: Binary,
    }
}

thrift_struct! {
    /// Union `ColumnCryptoMetaData`.
    ColumnCryptoMetaData {
        1: optional encryption_with_footer_key: EncryptionWithFooterKey,
        2: optional encryption_with_column_key: EncryptionWithColumnKey,
    }
}

thrift_struct! {
    /// A column of a row group (`ColumnChunk`).
    ColumnChunk {
        1: optional file_path: String,
        2: required file_offset: i64,
        3: optional meta_data: ColumnMetaData,
        4: optional offset_index_offset: i64,
        5: optional offset_index_length: i32,
        6: optional column_index_offset: i64,
        7: optional column_index_length: i32,
        8: optional crypto_metadata: ColumnCryptoMetaData,
        9: optional encrypted_column_metadata: Binary,
    }
}

thrift_struct! {
    /// `RowGroup`.
    RowGroup {
        1: required columns: Vec<ColumnChunk>,
        2: required total_byte_size: i64,
        3: required num_rows: i64,
        4: optional sorting_columns: Vec<SortingColumn>,
        5: optional file_offset: i64,
        6: optional total_compressed_size: i64,
        7: optional ordinal: i16,
    }
}

thrift_struct! {
    /// Union `ColumnOrder`.
    ColumnOrder {
        1: optional type_order: TypeDefinedOrder,
    }
}

thrift_struct! {
    /// `PageLocation`.
    PageLocation {
        1: required offset: i64,
        2: required compressed_page_size: i32,
        3: required first_row_index: i64,
    }
}

thrift_struct! {
    /// `OffsetIndex`.
    OffsetIndex {
        1: required page_locations: Vec<PageLocation>,
        2: optional unencoded_byte_array_data_bytes: Vec<i64>,
    }
}

thrift_struct! {
    /// `ColumnIndex`.
    ColumnIndex {
        1: required null_pages: Vec<bool>,
        2: required min_values: Vec<Binary>,
        3: required max_values: Vec<Binary>,
        4: required boundary_order: BoundaryOrder,
        5: optional null_counts: Vec<i64>,
        6: optional repetition_level_histograms: Vec<i64>,
        7: optional definition_level_histograms: Vec<i64>,
    }
}

thrift_struct! {
    /// `AesGcmV1`.
    AesGcmV1 {
        1: optional aad_prefix: Binary,
        2: optional aad_file_unique: Binary,
        3: optional supply_aad_prefix: bool,
    }
}

thrift_struct! {
    /// `AesGcmCtrV1`.
    AesGcmCtrV1 {
        1: optional aad_prefix: Binary,
        2: optional aad_file_unique: Binary,
        3: optional supply_aad_prefix: bool,
    }
}

thrift_struct! {
    /// Union `EncryptionAlgorithm`.
    EncryptionAlgorithm {
        1: optional aes_gcm_v1: AesGcmV1,
        2: optional aes_gcm_ctr_v1: AesGcmCtrV1,
    }
}

thrift_struct! {
    /// The file footer (`FileMetaData`).
    FileMetaData {
        1: required version: i32,
        2: required schema: Vec<SchemaElement>,
        3: required num_rows: i64,
        4: required row_groups: Vec<RowGroup>,
        5: optional key_value_metadata: Vec<KeyValue>,
        6: optional created_by: String,
        7: optional column_orders: Vec<ColumnOrder>,
        8: optional encryption_algorithm: EncryptionAlgorithm,
        9: optional footer_signing_key_metadata: Binary,
    }
}

thrift_struct! {
    /// The footer of a file with an encrypted footer (`FileCryptoMetaData`).
    FileCryptoMetaData {
        1: required encryption_algorithm: EncryptionAlgorithm,
        2: optional key_metadata: Binary,
    }
}
