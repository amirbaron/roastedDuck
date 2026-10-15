//! Scalar functions, organised like DuckDB's `src/function/scalar` and
//! `extension/core_functions/scalar`.

pub mod array;
pub mod date;
pub(crate) mod executor;
pub mod generic;
pub mod list;
pub mod math;
pub(crate) mod nested;
pub mod operator;
pub mod random;
pub mod string;
pub mod structs;

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
        structs::struct_pack_functions(),
        structs::struct_extract::struct_extract_functions(),
        structs::struct_extract::struct_extract_at_functions(),
        structs::struct_concat_functions(),
        structs::struct_insert_functions(),
        structs::struct_update_functions(),
        structs::struct_keys_functions(),
        structs::struct_values_functions(),
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
    sets.extend(date::to_interval::to_interval_functions());
    for (set, aliases) in math::numeric::numeric_functions() {
        with_aliases(&mut sets, set, aliases);
    }
    sets
}
