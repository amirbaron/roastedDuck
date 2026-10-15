//! `regexp_matches`, `regexp_full_match`, `regexp_replace`, `regexp_extract`,
//! `regexp_extract_all`, `regexp_escape` (DuckDB's `regexp.cpp`,
//! `regexp/regexp_extract_all.cpp`, `regexp_escape.cpp`) and `string_split`,
//! `string_split_regex` (`string_split.cpp`).

pub(crate) mod util;

use std::any::Any;
use std::sync::Arc;

use regex::bytes::Regex;
use rustdb_common::{Error, Result};
use rustdb_function::cast::ValueCast;
use rustdb_function::{BindInput, FunctionData, NullHandling, ScalarFunction, ScalarFunctionSet};
use rustdb_types::{DataChunk, ListEntry, LogicalType, LogicalTypeId, Value, Vector};

use crate::scalar::executor::{ScalarOutput, Str, StrOut, addressable, binary, ternary, unary};
use crate::scalar::structs::build_struct;
use util::{RegexOptions, compile, constant_pattern, group_count, parse_options_argument};

type BindResult = Result<Option<Arc<dyn FunctionData>>>;

/// What a `regexp_*` bind decided (DuckDB's `RegexpBaseBindData` and subclasses).
#[derive(Debug)]
struct RegexpBindData {
    options: RegexOptions,
    /// The compiled constant pattern, if the pattern argument is constant.
    constant: Option<Regex>,
    /// `regexp_replace`'s `g` option.
    global_replace: bool,
    /// `regexp_extract`'s group (`None` for a NULL group: always the empty string).
    group: Option<usize>,
    /// Group names of the STRUCT variants.
    names: Vec<String>,
}

impl FunctionData for RegexpBindData {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn bind_data(state: &rustdb_function::FunctionState) -> Result<&RegexpBindData> {
    state
        .bind_data
        .as_ref()
        .and_then(|data| data.as_any().downcast_ref::<RegexpBindData>())
        .ok_or_else(|| Error::internal("regexp function executed without bind data"))
}

/// A pattern compiled once per distinct value, for non-constant pattern columns.
#[derive(Default)]
struct PatternCache {
    last: Option<(Vec<u8>, Regex)>,
}

impl PatternCache {
    fn get(&mut self, pattern: &[u8], options: RegexOptions, full: bool) -> Result<&Regex> {
        let stale = self.last.as_ref().is_none_or(|(cached, _)| cached != pattern);
        if stale {
            self.last = Some((pattern.to_vec(), compile(pattern, options, full)?));
        }
        self.last
            .as_ref()
            .map(|(_, regex)| regex)
            .ok_or_else(|| Error::internal("pattern cache is empty"))
    }
}

fn varchar() -> LogicalType {
    LogicalType::VARCHAR
}

fn options_from(input: &BindInput<'_>, index: usize, global: Option<&mut bool>) -> Result<RegexOptions> {
    let mut options = RegexOptions::default();
    if input.argument_types.len() > index {
        parse_options_argument(
            &input.argument_types[index],
            input.constants.get(index),
            &mut options,
            global,
        )?;
    }
    Ok(options)
}

// ---------------------------------------------------------------------------------------
// regexp_matches / regexp_full_match
// ---------------------------------------------------------------------------------------

/// DuckDB's `RegexpMatchesBind` (also used by `string_split_regex`); `full` anchors the
/// constant pattern.
fn bind_matches(full: bool) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> BindResult {
    move |_, input| {
        let options = options_from(input, 2, None)?;
        let constant = constant_pattern(input.constants.get(1))
            .map(|pattern| compile(pattern.as_bytes(), options, full))
            .transpose()?;
        Ok(Some(Arc::new(RegexpBindData {
            options,
            constant,
            global_replace: false,
            group: None,
            names: Vec::new(),
        })))
    }
}

fn matches_kernel(
    full: bool,
) -> impl Fn(&DataChunk, &mut rustdb_function::FunctionState, &mut Vector) -> Result<()> {
    move |args, state, result| {
        let data = bind_data(state)?;
        if let Some(regex) = &data.constant {
            let strings = DataChunk::new(vec![args.column(0).clone()], args.len());
            return unary::<Str, bool>(&strings, result, |input, out| {
                *out = regex.is_match(input);
                Ok(true)
            });
        }
        let mut cache = PatternCache::default();
        let options = data.options;
        binary::<Str, Str, bool>(args, result, |input, pattern, out| {
            *out = cache.get(pattern, options, full)?.is_match(input);
            Ok(true)
        })
    }
}

fn matches_set(name: &str, full: bool) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    for arguments in [vec![varchar(), varchar()], vec![varchar(), varchar(), varchar()]] {
        let mut function = ScalarFunction::new(name, arguments, LogicalType::BOOLEAN, matches_kernel(full))
            .with_bind(bind_matches(full));
        function.null_handling = NullHandling::Special;
        set.add(function);
    }
    set
}

// ---------------------------------------------------------------------------------------
// regexp_replace
// ---------------------------------------------------------------------------------------

/// `RE2::Replace` (first match) or `RE2::GlobalReplace` into `out`.
fn replace(input: &[u8], regex: &Regex, replacement: &[u8], global: bool, out: &mut Vec<u8>) -> Result<()> {
    let groups = group_count(regex);
    let needed = util::max_submatch(replacement);
    if needed > groups {
        out.extend_from_slice(input);
        return Ok(());
    }
    if !global {
        let Some(captures) = regex.captures(input) else {
            out.extend_from_slice(input);
            return Ok(());
        };
        let whole = captures
            .get(0)
            .ok_or_else(|| Error::internal("match without group 0"))?;
        let mut rewritten = Vec::new();
        if !util::rewrite(&mut rewritten, replacement, &captures, needed + 1) {
            out.extend_from_slice(input);
            return Ok(());
        }
        out.extend_from_slice(&input[..whole.start()]);
        out.extend_from_slice(&rewritten);
        out.extend_from_slice(&input[whole.end()..]);
        return Ok(());
    }
    let mut position = 0;
    let mut last_end = None;
    let mut count = 0;
    let start = out.len();
    while position <= input.len() {
        let Some(captures) = regex.captures_at(input, position) else {
            break;
        };
        let whole = captures
            .get(0)
            .ok_or_else(|| Error::internal("match without group 0"))?;
        if position < whole.start() {
            out.extend_from_slice(&input[position..whole.start()]);
        }
        if Some(whole.start()) == last_end && whole.is_empty() {
            // An empty match right after the previous match: copy one character.
            let length = std::str::from_utf8(&input[position..])
                .ok()
                .and_then(|rest| rest.chars().next())
                .map_or(1, char::len_utf8);
            let end = (position + length).min(input.len());
            out.extend_from_slice(&input[position..end]);
            position += length;
            continue;
        }
        util::rewrite(out, replacement, &captures, needed + 1);
        position = whole.end();
        last_end = Some(position);
        count += 1;
    }
    if count == 0 {
        out.truncate(start);
        out.extend_from_slice(input);
        return Ok(());
    }
    if position < input.len() {
        out.extend_from_slice(&input[position..]);
    }
    Ok(())
}

fn bind_replace(_: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let mut global_replace = false;
    let options = options_from(input, 3, Some(&mut global_replace))?;
    let constant = constant_pattern(input.constants.get(1))
        .map(|pattern| compile(pattern.as_bytes(), options, false))
        .transpose()?;
    Ok(Some(Arc::new(RegexpBindData {
        options,
        constant,
        global_replace,
        group: None,
        names: Vec::new(),
    })))
}

fn replace_kernel(
    args: &DataChunk,
    state: &mut rustdb_function::FunctionState,
    result: &mut Vector,
) -> Result<()> {
    let data = bind_data(state)?;
    let global = data.global_replace;
    if let Some(regex) = &data.constant {
        let inputs = DataChunk::new(vec![args.column(0).clone(), args.column(2).clone()], args.len());
        return binary::<Str, Str, StrOut>(&inputs, result, |input, replacement, out| {
            replace(input, regex, replacement, global, out)?;
            Ok(true)
        });
    }
    let mut cache = PatternCache::default();
    let options = data.options;
    ternary::<Str, Str, Str, StrOut>(args, result, |input, pattern, replacement, out| {
        replace(
            input,
            cache.get(pattern, options, false)?,
            replacement,
            global,
            out,
        )?;
        Ok(true)
    })
}

fn replace_set() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("regexp_replace");
    for arguments in [
        vec![varchar(), varchar(), varchar()],
        vec![varchar(), varchar(), varchar(), varchar()],
    ] {
        set.add(
            ScalarFunction::new("regexp_replace", arguments, varchar(), replace_kernel)
                .with_bind(bind_replace),
        );
    }
    set
}

// ---------------------------------------------------------------------------------------
// regexp_extract
// ---------------------------------------------------------------------------------------

/// `RE2::Extract` with the rewrite `\N`: group `N` of the first match, or the empty string.
fn extract<'a>(input: &'a [u8], regex: &Regex, group: Option<usize>) -> &'a [u8] {
    let Some(group) = group else {
        return &[];
    };
    if group > group_count(regex) {
        return &[];
    }
    if group == 0 {
        return regex.find(input).map_or(&[], |found| found.as_bytes());
    }
    regex
        .captures(input)
        .and_then(|captures| captures.get(group))
        .map_or(&[], |found| found.as_bytes())
}

/// DuckDB's `ParseGroupNameList`: the STRUCT fields of the named-group variants.
fn parse_group_names(
    function_name: &str,
    argument_type: &LogicalType,
    constant: Option<&Option<Value>>,
    regex: Option<&Regex>,
) -> Result<Vec<String>> {
    if argument_type.id() == LogicalTypeId::Unknown {
        return Err(crate::parameter_not_resolved());
    }
    let Some(Some(value)) = constant else {
        return Err(Error::invalid_input(
            "Group specification field must be a constant list",
        ));
    };
    let children = (value.logical_type().id() == LogicalTypeId::List && !value.is_null())
        .then(|| value.children())
        .flatten();
    let Some(children) = children else {
        return Err(Error::binder("Group specification must be a non-NULL LIST"));
    };
    if children.is_empty() {
        return Err(Error::binder("Group name list must be non-empty"));
    }
    let mut names: Vec<String> = Vec::with_capacity(children.len());
    for child in children {
        if child.is_null() {
            return Err(Error::binder(format!("NULL group name in {function_name}")));
        }
        let name = crate::values::as_text(child)?;
        if names.iter().any(|existing| existing.eq_ignore_ascii_case(&name)) {
            return Err(Error::binder(format!(
                "Duplicate group name '{name}' in {function_name}"
            )));
        }
        names.push(name);
    }
    if let Some(regex) = regex {
        let groups = group_count(regex);
        if groups < names.len() {
            return Err(Error::binder(format!(
                "Not enough capturing groups ({groups}) for provided names ({})",
                names.len()
            )));
        }
    }
    Ok(names)
}

fn struct_type(names: &[String]) -> LogicalType {
    LogicalType::struct_type(names.iter().map(|name| (name.clone(), varchar())).collect())
}

/// DuckDB's `RegexExtractBind`.
fn bind_extract(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let options = options_from(input, 3, None)?;
    let pattern = constant_pattern(input.constants.get(1));
    let constant = pattern
        .as_ref()
        .map(|pattern| compile(pattern.as_bytes(), options, false))
        .transpose()?;
    let mut group = Some(0);
    let mut names = Vec::new();
    if input.argument_types.len() >= 3 {
        if input.argument_types[2].id() == LogicalTypeId::Unknown {
            return Err(crate::parameter_not_resolved());
        }
        let Some(Some(value)) = input.constants.get(2) else {
            return Err(Error::invalid_input(
                "Group specification field must be a constant!",
            ));
        };
        if value.is_null() {
            group = None;
        } else if value.logical_type().id() == LogicalTypeId::List {
            if constant.is_none() {
                return Err(Error::binder(format!(
                    "{} with LIST of group names requires a constant pattern",
                    function.name
                )));
            }
            names = parse_group_names(
                &function.name,
                &input.argument_types[2],
                input.constants.get(2),
                constant.as_ref(),
            )?;
            function.return_type = struct_type(&names);
            function.kernel = Arc::new(extract_struct_kernel);
        } else {
            let index = value
                .default_cast_as(&LogicalType::INTEGER)?
                .as_i32()
                .ok_or_else(|| Error::internal("INTEGER cast did not produce an integer"))?;
            if !(0..=9).contains(&index) {
                return Err(Error::invalid_input("Group index must be between 0 and 9!"));
            }
            group = Some(index as usize);
        }
    }
    Ok(Some(Arc::new(RegexpBindData {
        options,
        constant,
        global_replace: false,
        group,
        names,
    })))
}

fn extract_kernel(
    args: &DataChunk,
    state: &mut rustdb_function::FunctionState,
    result: &mut Vector,
) -> Result<()> {
    let data = bind_data(state)?;
    let group = data.group;
    if let Some(regex) = &data.constant {
        let strings = DataChunk::new(vec![args.column(0).clone()], args.len());
        return unary::<Str, StrOut>(&strings, result, |input, out| {
            out.extend_from_slice(extract(input, regex, group));
            Ok(true)
        });
    }
    let mut cache = PatternCache::default();
    let options = data.options;
    let inputs = DataChunk::new(vec![args.column(0).clone(), args.column(1).clone()], args.len());
    binary::<Str, Str, StrOut>(&inputs, result, |input, pattern, out| {
        // DuckDB does not check the pattern here: an invalid one extracts nothing.
        if let Ok(regex) = cache.get(pattern, options, false) {
            out.extend_from_slice(extract(input, regex, group));
        }
        Ok(true)
    })
}

/// Builds a VARCHAR vector of `rows` values.
fn varchar_vector(values: &[Option<Vec<u8>>]) -> Result<Vector> {
    let mut vector = Vector::flat(varchar(), values.len().max(1));
    StrOut::write(&mut vector, values.len(), |row, out| {
        Ok(match &values[row] {
            Some(value) => {
                out.extend_from_slice(value);
                true
            }
            None => false,
        })
    })?;
    Ok(vector)
}

/// `regexp_extract(string, pattern, [names])`: a STRUCT of the named groups (DuckDB's
/// `RegexExtractStructFunction`).
fn extract_struct_kernel(
    args: &DataChunk,
    state: &mut rustdb_function::FunctionState,
    result: &mut Vector,
) -> Result<()> {
    let data = bind_data(state)?;
    let regex = data.constant.as_ref().ok_or_else(|| {
        Error::internal("REGEXP_EXTRACT struct variant executed without constant pattern state")
    })?;
    let count = args.len();
    let strings = addressable(args.column(0), count);
    let constant = strings.is_constant();
    let rows = if constant { 1 } else { count };
    let unified = strings.unified::<rustdb_types::StringT>();
    let mut fields: Vec<Vec<Option<Vec<u8>>>> = vec![Vec::with_capacity(rows); data.names.len()];
    let mut nulls = Vec::new();
    for row in 0..rows {
        let Some(value) = unified.get(row) else {
            nulls.push(row);
            for field in &mut fields {
                field.push(Some(Vec::new()));
            }
            continue;
        };
        let input = strings.string_value(&value);
        let captures = regex.captures(input);
        for (index, field) in fields.iter_mut().enumerate() {
            let text = captures
                .as_ref()
                .and_then(|captures| captures.get(index + 1))
                .map_or(&[][..], |found| found.as_bytes());
            field.push(Some(text.to_vec()));
        }
    }
    let fields = fields
        .iter()
        .map(|field| varchar_vector(field))
        .collect::<Result<Vec<_>>>()?;
    let mut output = build_struct(result.logical_type().clone(), fields, rows, false)?;
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, rows);
    }
    if constant {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

fn extract_set() -> ScalarFunctionSet {
    let names = LogicalType::list(varchar());
    let mut set = ScalarFunctionSet::new("regexp_extract");
    for arguments in [
        vec![varchar(), varchar()],
        vec![varchar(), varchar(), LogicalType::INTEGER],
        vec![varchar(), varchar(), LogicalType::INTEGER, varchar()],
        vec![varchar(), varchar(), names.clone()],
        vec![varchar(), varchar(), names.clone(), varchar()],
    ] {
        let mut function = ScalarFunction::new("regexp_extract", arguments, varchar(), extract_kernel)
            .with_bind(bind_extract);
        function.null_handling = NullHandling::Special;
        set.add(function);
    }
    set
}

// ---------------------------------------------------------------------------------------
// regexp_extract_all
// ---------------------------------------------------------------------------------------

/// Every match of `regex` in `input` from DuckDB's `ExtractAll` loop: after an empty
/// advance, the search resumes one character later. Calls `each` with the captures.
fn for_each_match(
    input: &[u8],
    regex: &Regex,
    mut each: impl FnMut(&regex::bytes::Captures<'_>) -> Result<()>,
) -> Result<()> {
    let mut start = 0usize;
    loop {
        let Some(captures) = regex.captures_at(input, start) else {
            return Ok(());
        };
        let whole = captures
            .get(0)
            .ok_or_else(|| Error::internal("match without group 0"))?;
        let mut consumed = whole.end() - start;
        if consumed == 0 {
            consumed = util::advance_one_utf8(input, start)?;
        }
        start += consumed;
        each(&captures)?;
        if start > input.len() {
            return Ok(());
        }
    }
}

/// Builds a LIST(VARCHAR) vector from per-row lists (`None` = NULL list, NULL items).
fn varchar_lists(lists: &[Option<Vec<Option<Vec<u8>>>>], constant: bool) -> Result<Vector> {
    let rows = lists.len();
    let mut entries = Vec::with_capacity(rows);
    let mut items = Vec::new();
    let mut nulls = Vec::new();
    for (row, list) in lists.iter().enumerate() {
        match list {
            Some(list) => {
                entries.push(ListEntry::new(items.len(), list.len()));
                items.extend(list.iter().cloned());
            }
            None => {
                entries.push(ListEntry::new(items.len(), 0));
                nulls.push(row);
            }
        }
    }
    let child = varchar_vector(&items)?;
    let mut output = Vector::flat(LogicalType::list(varchar()), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, items.len());
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, rows);
    }
    if constant {
        output.make_constant();
    }
    Ok(output)
}

fn bind_extract_all(_: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let options = options_from(input, 3, None)?;
    let constant = constant_pattern(input.constants.get(1))
        .map(|pattern| compile(pattern.as_bytes(), options, false))
        .transpose()?;
    Ok(Some(Arc::new(RegexpBindData {
        options,
        constant,
        global_replace: false,
        group: Some(0),
        names: Vec::new(),
    })))
}

/// DuckDB's `RegexpExtractAll::Execute`.
fn extract_all_kernel(
    args: &DataChunk,
    state: &mut rustdb_function::FunctionState,
    result: &mut Vector,
) -> Result<()> {
    let data = bind_data(state)?;
    let count = args.len();
    let columns: Vec<_> = args
        .columns()
        .iter()
        .map(|column| addressable(column, count))
        .collect();
    let constant = columns.iter().all(|column| column.is_constant());
    let rows = if constant { 1 } else { count };
    let strings = columns[0].unified::<rustdb_types::StringT>();
    let patterns = columns[1].unified::<rustdb_types::StringT>();
    let groups = columns.get(2).map(|column| column.unified::<i32>());
    let mut cache = PatternCache::default();
    let mut lists = Vec::with_capacity(rows);
    for row in 0..rows {
        let regex = match &data.constant {
            Some(regex) => Some(regex),
            None => match patterns.get(row) {
                Some(pattern) => Some(
                    cache
                        .get(columns[1].string_value(&pattern), data.options, false)
                        .map_err(|error| {
                            Error::invalid_input(format!(
                                "Pattern failed to parse, error: '{}'",
                                error.raw_message()
                            ))
                        })?,
                ),
                None => None,
            },
        };
        let group = match &groups {
            Some(groups) => groups.get(row),
            None => Some(0),
        };
        let (Some(regex), Some(string), Some(group)) = (regex, strings.get(row), group) else {
            lists.push(None);
            continue;
        };
        let input = columns[0].string_value(&string);
        let mut items = Vec::new();
        if let Ok(group) = usize::try_from(group) {
            let available = group_count(regex);
            for_each_match(input, regex, |captures| {
                if group > available {
                    return Err(Error::invalid_input(format!(
                        "Pattern has {available} groups. Cannot access group {group}"
                    )));
                }
                items.push(captures.get(group).map(|found| found.as_bytes().to_vec()));
                Ok(())
            })?;
        }
        lists.push(Some(items));
    }
    *result = varchar_lists(&lists, constant)?;
    Ok(())
}

fn bind_extract_all_struct(function: &mut ScalarFunction, input: &BindInput<'_>) -> BindResult {
    let Some(pattern) = constant_pattern(input.constants.get(1)) else {
        return Err(Error::binder(format!(
            "{} with LIST requires a constant pattern",
            function.name
        )));
    };
    let options = options_from(input, 3, None)?;
    let regex = compile(pattern.as_bytes(), options, false)?;
    let names = parse_group_names(
        &function.name,
        &input.argument_types[2],
        input.constants.get(2),
        Some(&regex),
    )?;
    function.return_type = LogicalType::list(struct_type(&names));
    Ok(Some(Arc::new(RegexpBindData {
        options,
        constant: Some(regex),
        global_replace: false,
        group: None,
        names,
    })))
}

/// DuckDB's `RegexpExtractAllStruct::Execute`: a list of STRUCTs of the named groups.
fn extract_all_struct_kernel(
    args: &DataChunk,
    state: &mut rustdb_function::FunctionState,
    result: &mut Vector,
) -> Result<()> {
    let data = bind_data(state)?;
    let regex = data
        .constant
        .as_ref()
        .ok_or_else(|| Error::internal("regexp_extract_all struct variant without constant pattern"))?;
    let count = args.len();
    let strings = addressable(args.column(0), count);
    let constant = args.columns().iter().all(Vector::is_constant);
    let rows = if constant { 1 } else { count };
    let unified = strings.unified::<rustdb_types::StringT>();
    let mut fields: Vec<Vec<Option<Vec<u8>>>> = vec![Vec::new(); data.names.len()];
    let mut entries = Vec::with_capacity(rows);
    let mut nulls = Vec::new();
    let mut size = 0;
    for row in 0..rows {
        let Some(value) = unified.get(row) else {
            entries.push(ListEntry::new(size, 0));
            nulls.push(row);
            continue;
        };
        let input = strings.string_value(&value);
        let start = size;
        for_each_match(input, regex, |captures| {
            for (index, field) in fields.iter_mut().enumerate() {
                field.push(captures.get(index + 1).map(|found| found.as_bytes().to_vec()));
            }
            size += 1;
            Ok(())
        })?;
        entries.push(ListEntry::new(start, size - start));
    }
    let child_type = crate::scalar::nested::list_child_type(result.logical_type())?;
    let field_vectors = fields
        .iter()
        .map(|field| varchar_vector(field))
        .collect::<Result<Vec<_>>>()?;
    let child = build_struct(child_type, field_vectors, size, false)?;
    let mut output = Vector::flat(result.logical_type().clone(), rows.max(1));
    output.values_mut::<ListEntry>()[..rows].copy_from_slice(&entries);
    output.set_list_child(child, size);
    let validity = output.validity_mut();
    for row in nulls {
        validity.set_invalid(row, rows);
    }
    if constant {
        output.make_constant();
    }
    *result = output;
    Ok(())
}

fn extract_all_set() -> ScalarFunctionSet {
    let list = LogicalType::list(varchar());
    let mut set = ScalarFunctionSet::new("regexp_extract_all");
    for arguments in [
        vec![varchar(), varchar()],
        vec![varchar(), varchar(), LogicalType::INTEGER],
        vec![varchar(), varchar(), LogicalType::INTEGER, varchar()],
    ] {
        let mut function =
            ScalarFunction::new("regexp_extract_all", arguments, list.clone(), extract_all_kernel)
                .with_bind(bind_extract_all);
        function.null_handling = NullHandling::Special;
        set.add(function);
    }
    for arguments in [
        vec![varchar(), varchar(), list.clone()],
        vec![varchar(), varchar(), list.clone(), varchar()],
    ] {
        let mut function = ScalarFunction::new(
            "regexp_extract_all",
            arguments,
            list.clone(),
            extract_all_struct_kernel,
        )
        .with_bind(bind_extract_all_struct);
        function.null_handling = NullHandling::Special;
        set.add(function);
    }
    set
}

// ---------------------------------------------------------------------------------------
// regexp_escape, string_split, string_split_regex
// ---------------------------------------------------------------------------------------

fn escape_set() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("regexp_escape");
    set.add(ScalarFunction::new(
        "regexp_escape",
        vec![varchar()],
        varchar(),
        |args, _, result| {
            unary::<Str, StrOut>(args, result, |input, out| {
                util::quote_meta(input, out);
                Ok(true)
            })
        },
    ));
    set
}

/// DuckDB's `StringSplitter::Split`; `find` returns the next delimiter's offset and size
/// within the remaining input.
fn split<'a>(
    mut input: &'a [u8],
    mut find: impl FnMut(&[u8]) -> Result<Option<(usize, usize)>>,
) -> Result<Vec<&'a [u8]>> {
    let mut parts = Vec::new();
    while !input.is_empty() {
        let Some((mut position, size)) = find(input)? else {
            break;
        };
        if size == 0 && position == 0 {
            position = 1;
            while position < input.len() && input[position] & 0xc0 == 0x80 {
                position += 1;
            }
            if position == input.len() {
                break;
            }
        }
        parts.push(&input[..position]);
        input = &input[position + size..];
    }
    parts.push(input);
    Ok(parts)
}

/// Runs a splitter over (string, delimiter) rows (DuckDB's `StringSplitExecutor`): a NULL
/// string gives NULL, a NULL delimiter the whole string.
fn split_executor(
    args: &DataChunk,
    result: &mut Vector,
    mut split_row: impl FnMut(&[u8], &[u8]) -> Result<Vec<Vec<u8>>>,
) -> Result<()> {
    let count = args.len();
    let strings = addressable(args.column(0), count);
    let delimiters = addressable(args.column(1), count);
    let constant = args.columns().iter().all(Vector::is_constant);
    let rows = if constant { 1 } else { count };
    let string_values = strings.unified::<rustdb_types::StringT>();
    let delimiter_values = delimiters.unified::<rustdb_types::StringT>();
    let mut lists = Vec::with_capacity(rows);
    for row in 0..rows {
        let Some(string) = string_values.get(row) else {
            lists.push(None);
            continue;
        };
        let input = strings.string_value(&string);
        let parts = match delimiter_values.get(row) {
            Some(delimiter) => split_row(input, delimiters.string_value(&delimiter))?,
            None => vec![input.to_vec()],
        };
        lists.push(Some(parts.into_iter().map(Some).collect()));
    }
    *result = varchar_lists(&lists, constant)?;
    Ok(())
}

fn split_set() -> ScalarFunctionSet {
    let mut function = ScalarFunction::new(
        "string_split",
        vec![varchar(), varchar()],
        LogicalType::list(varchar()),
        |args, _, result| {
            split_executor(args, result, |input, delimiter| {
                let parts = split(input, |rest| {
                    if delimiter.is_empty() {
                        return Ok(Some((0, 0)));
                    }
                    Ok(crate::scalar::string::contains::find(rest, delimiter)
                        .map(|position| (position, delimiter.len())))
                })?;
                Ok(parts.into_iter().map(<[u8]>::to_vec).collect())
            })
        },
    );
    function.null_handling = NullHandling::Special;
    let mut set = ScalarFunctionSet::new("string_split");
    set.add(function);
    set
}

fn regex_split(input: &[u8], regex: &Regex) -> Result<Vec<Vec<u8>>> {
    let parts = split(input, |rest| {
        Ok(regex.find(rest).map(|found| (found.start(), found.len())))
    })?;
    Ok(parts.into_iter().map(<[u8]>::to_vec).collect())
}

fn split_regex_set() -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new("string_split_regex");
    for arguments in [vec![varchar(), varchar()], vec![varchar(), varchar(), varchar()]] {
        let mut function = ScalarFunction::new(
            "string_split_regex",
            arguments,
            LogicalType::list(varchar()),
            |args, state, result| {
                let data = bind_data(state)?;
                if let Some(regex) = &data.constant {
                    return split_executor(args, result, |input, _| regex_split(input, regex));
                }
                // DuckDB compiles a non-constant delimiter with the default options.
                let mut cache = PatternCache::default();
                split_executor(args, result, |input, delimiter| {
                    regex_split(input, cache.get(delimiter, RegexOptions::default(), false)?)
                })
            },
        )
        .with_bind(bind_matches(false));
        function.null_handling = NullHandling::Special;
        set.add(function);
    }
    set
}

/// Every regular expression function set with its aliases.
pub fn regexp_functions() -> Vec<(ScalarFunctionSet, &'static [&'static str])> {
    vec![
        (matches_set("regexp_full_match", true), &[]),
        (matches_set("regexp_matches", false), &[]),
        (replace_set(), &[]),
        (extract_set(), &[]),
        (extract_all_set(), &[]),
        (escape_set(), &[]),
        (split_set(), &["str_split", "string_to_array", "split"]),
        (split_regex_set(), &["str_split_regex", "regexp_split_to_array"]),
    ]
}
