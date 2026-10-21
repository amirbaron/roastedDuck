//! Scalar functions, organised like DuckDB's `src/function/scalar` and
//! `extension/core_functions/scalar`.

pub mod aggregate_export;
pub mod array;
pub mod binning;
pub mod bit;
pub mod blob;
pub mod compressed_materialization;
pub mod date;
pub mod enums;
pub(crate) mod executor;
pub mod generic;
pub mod geometry;
pub mod list;
pub mod log;
pub mod map;
pub mod math;
pub(crate) mod nested;
pub mod operator;
pub mod random;
pub mod string;
pub mod structs;
pub mod system;
pub mod union;
pub mod variant;

use rustdb_function::ScalarFunctionSet;

use operator::arithmetic;

/// A copy of `set` under another name, with every overload renamed (DuckDB registers
/// aliases as separate catalog entries).
pub(crate) fn alias(set: &ScalarFunctionSet, name: &str) -> ScalarFunctionSet {
    let name: std::sync::Arc<str> = std::sync::Arc::from(name);
    let mut alias = set.clone();
    alias.name = std::sync::Arc::clone(&name);
    for function in &mut alias.functions {
        function.name = std::sync::Arc::clone(&name);
    }
    alias
}

/// Adds `set` and its aliases to `sets`.
fn with_aliases(sets: &mut Vec<ScalarFunctionSet>, set: ScalarFunctionSet, aliases: &[&str]) {
    for name in aliases {
        sets.push(alias(&set, name));
    }
    sets.push(set);
}

/// Every builtin scalar function set, aliases included.
pub fn scalar_functions() -> Vec<ScalarFunctionSet> {
    let mut sets = vec![
        arithmetic::divide_functions(),
        crate::aggregate::nested::binned_histogram::is_histogram_other_bin_function_set(),
        list::list_value::unpivot_list_functions(),
        structs::row_functions(),
        structs::remap_struct::remap_struct_functions(),
        structs::struct_pack_functions(),
        structs::struct_extract::struct_extract_functions(),
        structs::struct_extract::struct_extract_at_functions(),
        structs::struct_concat_functions(),
        structs::struct_insert_functions(),
        structs::struct_update_functions(),
        structs::struct_keys_functions(),
        structs::struct_values_functions(),
        union::union_extract_functions(),
        union::union_tag_functions(),
        union::union_value_functions(),
        binning::equi_width_bins_functions(),
        enums::enum_first_functions(),
        enums::enum_last_functions(),
        enums::enum_code_functions(),
        enums::enum_range_functions(),
        enums::enum_range_boundary_functions(),
        list::list_extract::array_extract_functions(),
        array::array_value::array_value_functions(),
        operator::bitwise::bitwise_and_functions(),
        operator::bitwise::bitwise_or_functions(),
        operator::bitwise::bitwise_xor_functions(),
        operator::bitwise::bitwise_not_functions(),
        operator::bitwise::left_shift_functions(),
        operator::bitwise::right_shift_functions(),
        random::random_functions(),
        random::setseed_functions(),
        random::uuid_functions("uuidv4"),
        random::uuidv7_functions(),
        random::uuid_extract_version_functions(),
        random::uuid_extract_timestamp_functions(),
    ];
    with_aliases(&mut sets, random::uuid_functions("uuid"), &["gen_random_uuid"]);
    string_functions(&mut sets);
    with_aliases(&mut sets, arithmetic::add_functions(), &["add"]);
    with_aliases(&mut sets, arithmetic::subtract_functions(), &["subtract"]);
    with_aliases(&mut sets, arithmetic::multiply_functions(), &["multiply"]);
    with_aliases(&mut sets, arithmetic::integer_divide_functions(), &["divide"]);
    with_aliases(&mut sets, arithmetic::modulo_functions(), &["mod"]);
    with_aliases(
        &mut sets,
        list::list_value::list_value_functions(),
        &["list_pack"],
    );
    with_aliases(
        &mut sets,
        list::list_extract::list_extract_functions(),
        &["list_element"],
    );
    with_aliases(&mut sets, structs::struct_contains_functions(), &["struct_has"]);
    with_aliases(
        &mut sets,
        structs::struct_position_functions(),
        &["struct_indexof"],
    );
    with_aliases(&mut sets, string::substring::substring_functions(), &["substr"]);
    sets.push(string::substring::substring_grapheme_functions());
    sets.extend(generic::generic_functions());
    for (set, aliases) in geometry::geometry_functions() {
        with_aliases(&mut sets, set, aliases);
    }
    sets.extend(variant::variant_functions());
    sets.extend(log::log_functions());
    sets.extend(system::system_functions());
    sets.extend(aggregate_export::aggregate_export_functions());
    sets.extend(compressed_materialization::compressed_materialization_functions());
    sets.extend(date::to_interval::to_interval_functions());
    for (set, aliases) in date::date_part::named_part_functions() {
        with_aliases(&mut sets, set, aliases);
    }
    with_aliases(
        &mut sets,
        date::date_trunc::date_trunc_functions(),
        &["datetrunc"],
    );
    with_aliases(&mut sets, date::date_diff::date_diff_functions(), &["datediff"]);
    with_aliases(&mut sets, date::date_diff::date_sub_functions(), &["datesub"]);
    sets.push(date::date_diff::age_functions());
    sets.push(date::time_bucket::time_bucket_functions());
    sets.push(date::strftime::strftime_functions());
    with_aliases(
        &mut sets,
        date::current::current_timestamp_functions(),
        &["now", "transaction_timestamp"],
    );
    sets.extend(date::strftime::strptime_functions());
    sets.push(date::make_date::make_date_functions());
    sets.push(date::make_date::make_time_functions());
    sets.push(date::make_date::make_timestamp_functions());
    sets.push(date::make_date::make_timestamp_ns_functions());
    sets.push(date::make_date::to_timestamp_functions());
    sets.extend(date::make_date::interval_helper_functions());
    for (set, aliases) in math::numeric::numeric_functions() {
        with_aliases(&mut sets, set, aliases);
    }
    merge_overloads(sets)
}

/// Sets registered under the same name become one set, the overloads of the first
/// registration first (DuckDB registers built-ins with `ALTER_ON_CONFLICT`, merging
/// them; e.g. `&&` of GEOMETRY and of lists).
fn merge_overloads(sets: Vec<ScalarFunctionSet>) -> Vec<ScalarFunctionSet> {
    let mut merged: Vec<ScalarFunctionSet> = Vec::with_capacity(sets.len());
    let mut positions: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for set in sets {
        let name = set.name.to_lowercase();
        if let Some(&position) = positions.get(&name) {
            merged[position].functions.extend(set.functions);
        } else {
            positions.insert(name, merged.len());
            merged.push(set);
        }
    }
    merged
}

/// The string functions (DuckDB's `function/scalar/string` and
/// `core_functions/scalar/string`).
fn string_functions(sets: &mut Vec<ScalarFunctionSet>) {
    use string::{
        caseconvert, character, concat, contains, encode, hash, length, normalize, replace, reverse, trim_pad,
    };
    with_aliases(sets, caseconvert::upper_functions(), &["ucase"]);
    with_aliases(sets, caseconvert::lower_functions(), &["lcase"]);
    sets.push(concat::concat_ws_functions());
    sets.push(concat::concat_functions());
    with_aliases(
        sets,
        concat::list_concat_functions(),
        &["list_cat", "array_concat", "array_cat"],
    );
    sets.push(concat::concat_operator_functions());
    sets.push(contains::prefix_functions());
    with_aliases(sets, contains::suffix_functions(), &["ends_with"]);
    let mut nested_contains = vec![
        list::contains_or_position::list_contains_function("contains"),
        list::contains_or_position::map_contains_function("contains"),
    ];
    nested_contains.extend(structs::struct_contains_functions().functions);
    sets.push(contains::contains_functions(nested_contains));
    with_aliases(
        sets,
        list::contains_or_position::list_contains_functions(),
        &["array_contains", "list_has", "array_has"],
    );
    with_aliases(
        sets,
        list::contains_or_position::list_position_functions(),
        &["list_indexof", "array_position", "array_indexof"],
    );
    sets.push(list::contains_or_position::map_contains_functions());
    for (set, aliases) in list::list_sort::list_sort_functions()
        .into_iter()
        .chain(list::list_aggregates::list_distinct_functions())
    {
        with_aliases(sets, set, aliases);
    }
    sets.extend(list::range::range_functions());
    with_aliases(
        sets,
        list::list_aggregates::list_aggregate_functions(),
        &["array_aggregate", "list_aggr", "array_aggr", "aggregate"],
    );
    with_aliases(sets, list::slice::list_slice_functions(), &["array_slice"]);
    sets.push(list::slice::flatten_functions());
    for (set, aliases) in list::list_ops::list_select_functions() {
        with_aliases(sets, set, aliases);
    }
    with_aliases(sets, list::list_ops::list_resize_functions(), &["array_resize"]);
    with_aliases(sets, list::list_ops::list_zip_functions(), &["array_zip"]);
    with_aliases(
        sets,
        list::list_ops::list_intersect_functions(),
        &["array_intersect"],
    );
    for (set, aliases) in list::lambda::lambda_functions() {
        with_aliases(sets, set, aliases);
    }
    for (set, aliases) in list::has_any_or_all::has_any_or_all_functions()
        .into_iter()
        .chain(array::array_functions::distance_functions())
    {
        with_aliases(sets, set, aliases);
    }
    sets.push(map::map_functions());
    sets.push(map::cardinality_functions());
    sets.extend(map::map_unpack_functions());
    let (map_extract, map_extract_value) = map::map_extract_functions();
    with_aliases(sets, map_extract, &["element_at"]);
    sets.push(map_extract_value);
    sets.push(map::map_from_entries_functions());
    sets.push(map::map_concat_functions());
    with_aliases(sets, contains::starts_with_functions(), &["starts_with"]);
    sets.push(normalize::strip_accents_functions());
    sets.push(normalize::nfc_normalize_functions());
    with_aliases(
        sets,
        length::length_functions("length"),
        &["len", "char_length", "character_length"],
    );
    sets.push(length::strlen_functions());
    sets.push(length::bit_length_functions());
    sets.push(length::octet_length_functions());
    sets.push(length::length_grapheme_functions());
    sets.push(length::array_length_functions());
    sets.push(hash::md5_functions());
    sets.push(hash::md5_number_functions());
    sets.push(hash::sha1_functions());
    sets.push(hash::sha256_functions());
    sets.push(character::ascii_functions());
    sets.push(character::chr_functions());
    with_aliases(sets, character::unicode_functions(), &["ord"]);
    sets.push(reverse::reverse_functions());
    sets.push(replace::repeat_functions());
    sets.push(replace::replace_functions());
    sets.extend(string::like::like_functions());
    for (set, aliases) in string::regexp::regexp_functions() {
        with_aliases(sets, set, aliases);
    }
    for (set, aliases) in string::similarity::similarity_functions() {
        with_aliases(sets, set, aliases);
    }
    sets.extend(trim_pad::left_right_functions());
    sets.extend(trim_pad::pad_functions());
    sets.extend(trim_pad::trim_functions());
    with_aliases(sets, trim_pad::instr_functions(), &["strpos", "position"]);
    sets.push(trim_pad::translate_functions());
    with_aliases(sets, encode::hex_functions(), &["to_hex"]);
    with_aliases(sets, encode::bin_functions(), &["to_binary"]);
    let (unhex, unbin) = encode::unhex_unbin_functions();
    with_aliases(sets, unhex, &["from_hex"]);
    with_aliases(sets, unbin, &["from_binary"]);
    sets.push(encode::to_base_functions());
    sets.extend(encode::url_functions());
    let (format_bytes, decimal_size) = encode::format_bytes_functions();
    with_aliases(sets, format_bytes, &["formatReadableSize"]);
    sets.push(decimal_size);
    sets.push(encode::parse_formatted_bytes_functions());
    sets.push(encode::bar_functions());
    sets.extend(string::parse_path::parse_path_functions());
    sets.push(string::printf::printf_functions());
    sets.push(string::printf::format_functions());
    with_aliases(sets, blob::base64::to_base64_functions(), &["base64"]);
    sets.push(blob::base64::from_base64_functions());
    sets.push(blob::encode::encode_functions());
    sets.push(blob::encode::decode_functions());
    sets.push(bit::bitstring_functions());
    sets.push(bit::get_bit_functions());
    sets.push(bit::set_bit_functions());
    sets.push(bit::bit_position_functions());
}
