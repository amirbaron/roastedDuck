//! DuckDB's `icu-datepart.cpp`: the date part functions (`year`, ..., `epoch`, `julian`,
//! `date_part` and its STRUCT form, `last_day`, `monthname`, `dayname`) over TIMESTAMPTZ
//! in the session's calendar and time zone.

use std::any::Any;
use std::sync::Arc;

use rustdb_common::{Error, Result};
use rustdb_function::{BindInput, FunctionData, FunctionState, ScalarFunction, ScalarFunctionSet};
use rustdb_functions::scalar::date::calendar::{DAY_NAMES, MONTH_NAMES};
use rustdb_functions::scalar::date::date_part::Part;
use rustdb_functions::scalar::executor::{Str, StrOut, addressable, binary, unary};
use rustdb_functions::scalar::structs::build_struct;
use rustdb_types::{DataChunk, Date, LogicalType, LogicalTypeId, NativeType, Timestamp, Vector};

use crate::icu_datefunc::{self, BindData, CalendarPtr, MICROS_PER_MSEC, extract_field, set_time};
use crate::icu4c::calendar::{Calendar, Status, field};
use crate::icu4c::grego::{MONDAY, SUNDAY};

/// `ICUDatePart::part_bigint_t`.
type PartBigint = fn(&mut Calendar, u64) -> Result<i64>;
/// `ICUDatePart::part_double_t`.
type PartDouble = fn(&mut Calendar, u64) -> Result<f64>;

// Date part adapters

fn extract_era(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    Ok(i64::from(extract_field(calendar, field::ERA)?))
}

fn extract_year(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    Ok(i64::from(extract_field(calendar, field::YEAR)?))
}

fn extract_decade(calendar: &mut Calendar, micros: u64) -> Result<i64> {
    Ok(extract_year(calendar, micros)? / 10)
}

fn extract_century(calendar: &mut Calendar, micros: u64) -> Result<i64> {
    let era = extract_era(calendar, micros)?;
    let cccc = ((extract_year(calendar, micros)? - 1) / 100) + 1;
    Ok(if era > 0 { cccc } else { -cccc })
}

fn extract_millenium(calendar: &mut Calendar, micros: u64) -> Result<i64> {
    let era = extract_era(calendar, micros)?;
    let mmmm = ((extract_year(calendar, micros)? - 1) / 1000) + 1;
    Ok(if era > 0 { mmmm } else { -mmmm })
}

fn extract_month(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    Ok(i64::from(extract_field(calendar, field::MONTH)?) + 1)
}

fn extract_quarter(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    Ok(i64::from(extract_field(calendar, field::MONTH)?) / 3 + 1)
}

fn extract_day(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    Ok(i64::from(extract_field(calendar, field::DATE)?))
}

fn extract_day_of_week(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    // [Sun(0), Sat(6)]
    Ok(i64::from(extract_field(calendar, field::DAY_OF_WEEK)? - SUNDAY))
}

fn extract_iso_day_of_week(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    // [Mon(1), Sun(7)]
    Ok(i64::from(
        1 + (extract_field(calendar, field::DAY_OF_WEEK)? + 7 - MONDAY) % 7,
    ))
}

fn extract_week(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    calendar.set_first_day_of_week(MONDAY);
    calendar.set_minimal_days_in_first_week(4);
    Ok(i64::from(extract_field(calendar, field::WEEK_OF_YEAR)?))
}

fn extract_iso_year(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    calendar.set_first_day_of_week(MONDAY);
    calendar.set_minimal_days_in_first_week(4);
    Ok(i64::from(extract_field(calendar, field::YEAR_WOY)?))
}

fn extract_year_week(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    calendar.set_first_day_of_week(MONDAY);
    calendar.set_minimal_days_in_first_week(4);
    let iyyy = i64::from(extract_field(calendar, field::YEAR_WOY)?);
    let ww = i64::from(extract_field(calendar, field::WEEK_OF_YEAR)?);
    Ok(iyyy * 100 + if iyyy > 0 { ww } else { -ww })
}

fn extract_day_of_year(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    Ok(i64::from(extract_field(calendar, field::DAY_OF_YEAR)?))
}

fn extract_hour(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    Ok(i64::from(extract_field(calendar, field::HOUR_OF_DAY)?))
}

fn extract_minute(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    Ok(i64::from(extract_field(calendar, field::MINUTE)?))
}

fn extract_second(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    Ok(i64::from(extract_field(calendar, field::SECOND)?))
}

fn extract_millisecond(calendar: &mut Calendar, micros: u64) -> Result<i64> {
    Ok(extract_second(calendar, micros)? * 1000 + i64::from(extract_field(calendar, field::MILLISECOND)?))
}

fn extract_microsecond(calendar: &mut Calendar, micros: u64) -> Result<i64> {
    Ok(extract_millisecond(calendar, micros)? * MICROS_PER_MSEC + micros as i64)
}

#[allow(clippy::unnecessary_wraps)] // the signature of the function pointer it is used as
fn extract_epoch(calendar: &mut Calendar, micros: u64) -> Result<f64> {
    let mut status = Status::ok();
    let mut result = calendar.get_time_in_millis(&mut status) / 1000.0;
    result += micros as f64 / 1_000_000.0;
    Ok(result)
}

fn extract_timezone(calendar: &mut Calendar, _micros: u64) -> Result<i64> {
    let mut millis = extract_field(calendar, field::ZONE_OFFSET)?;
    millis += extract_field(calendar, field::DST_OFFSET)?;
    Ok(i64::from(millis / 1000))
}

fn extract_timezone_hour(calendar: &mut Calendar, micros: u64) -> Result<i64> {
    let secs = extract_timezone(calendar, micros)?;
    Ok(secs / 3600)
}

fn extract_timezone_minute(calendar: &mut Calendar, micros: u64) -> Result<i64> {
    let secs = extract_timezone(calendar, micros)?;
    Ok((secs % 3600) / 60)
}

/// PG uses doubles for JDs so we can only use them with other double types.
fn extract_julian_day(calendar: &mut Calendar, micros: u64) -> Result<f64> {
    // We need days + fraction
    let days = extract_field(calendar, field::JULIAN_DAY)?;
    let mut frac = extract_hour(calendar, micros)?;

    frac *= 60;
    frac += extract_minute(calendar, micros)?;

    frac *= 60_000_000;
    frac += extract_microsecond(calendar, micros)?;

    let mut result = frac as f64;
    result /= 86_400_000_000.0;
    result += f64::from(days);
    Ok(result)
}

/// `ICUDatePart::PartCodeBigintFactory`.
fn part_code_bigint_factory(part: Part) -> Result<PartBigint> {
    Ok(match part {
        Part::Year => extract_year,
        Part::Month => extract_month,
        Part::Day => extract_day,
        Part::Decade => extract_decade,
        Part::Century => extract_century,
        Part::Millennium => extract_millenium,
        Part::Microseconds => extract_microsecond,
        Part::Milliseconds => extract_millisecond,
        Part::Second => extract_second,
        Part::Minute => extract_minute,
        Part::Hour => extract_hour,
        Part::Dow => extract_day_of_week,
        Part::IsoDow => extract_iso_day_of_week,
        Part::Week => extract_week,
        Part::IsoYear => extract_iso_year,
        Part::Doy => extract_day_of_year,
        Part::Quarter => extract_quarter,
        Part::YearWeek => extract_year_week,
        Part::Era => extract_era,
        Part::Timezone => extract_timezone,
        Part::TimezoneHour => extract_timezone_hour,
        Part::TimezoneMinute => extract_timezone_minute,
        Part::Epoch | Part::JulianDay => return Err(Error::internal("Unsupported ICU BIGINT extractor")),
    })
}

/// `ICUDatePart::PartCodeDoubleFactory`.
fn part_code_double_factory(part: Part) -> Result<PartDouble> {
    match part {
        Part::Epoch => Ok(extract_epoch),
        Part::JulianDay => Ok(extract_julian_day),
        _ => Err(Error::internal("Unsupported ICU DOUBLE extractor")),
    }
}

/// `ICUDatePart::MakeLastDay`: midnight on the last day of the month.
fn make_last_day(calendar: &mut Calendar, _micros: u64) -> Result<Date> {
    // Set the calendar to midnight on the last day of the month
    calendar.set(field::MILLISECOND, 0);
    calendar.set(field::SECOND, 0);
    calendar.set(field::MINUTE, 0);
    calendar.set(field::HOUR_OF_DAY, 0);

    let mut status = Status::ok();
    let dd = calendar.get_actual_maximum(field::DATE, &mut status);
    if status.is_failure() {
        return Err(Error::internal("Unable to extract ICU last day."));
    }

    calendar.set(field::DATE, dd);

    // Offset to UTC
    let mut millis = calendar.get_time_in_millis(&mut status);
    millis += f64::from(extract_field(calendar, field::ZONE_OFFSET)?);
    millis += f64::from(extract_field(calendar, field::DST_OFFSET)?);

    // `Date::EpochToDate`
    let seconds = (millis / 1000.0) as i64;
    Ok(Date(seconds.div_euclid(86_400) as i32))
}

fn month_name(calendar: &mut Calendar, micros: u64, out: &mut Vec<u8>) -> Result<()> {
    let mm = extract_month(calendar, micros)? - 1;
    if mm == 12 {
        out.extend_from_slice(b"Undecimber");
    } else {
        out.extend_from_slice(MONTH_NAMES[mm as usize].as_bytes());
    }
    Ok(())
}

fn day_name(calendar: &mut Calendar, micros: u64, out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(DAY_NAMES[extract_day_of_week(calendar, micros)? as usize].as_bytes());
    Ok(())
}

/// What a unary part function computes (`BindAdapterData::adapters`).
#[derive(Clone, Copy)]
enum Adapter {
    Bigint(PartBigint),
    Double(PartDouble),
    Date(fn(&mut Calendar, u64) -> Result<Date>),
    Text(fn(&mut Calendar, u64, &mut Vec<u8>) -> Result<()>),
}

impl std::fmt::Debug for Adapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Bigint(_) => "Bigint",
            Self::Double(_) => "Double",
            Self::Date(_) => "Date",
            Self::Text(_) => "Text",
        })
    }
}

/// `ICUDatePart::BindAdapterData`.
#[derive(Debug)]
struct BindAdapterData {
    data: BindData,
    adapter: Option<Adapter>,
}

impl FunctionData for BindAdapterData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other.as_any().downcast_ref::<Self>().is_some_and(|other| {
            self.data.equals(&other.data)
                && match (self.adapter, other.adapter) {
                    (Some(Adapter::Bigint(a)), Some(Adapter::Bigint(b))) => std::ptr::fn_addr_eq(a, b),
                    (Some(Adapter::Double(a)), Some(Adapter::Double(b))) => std::ptr::fn_addr_eq(a, b),
                    (Some(Adapter::Date(a)), Some(Adapter::Date(b))) => std::ptr::fn_addr_eq(a, b),
                    (Some(Adapter::Text(a)), Some(Adapter::Text(b))) => std::ptr::fn_addr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                }
        })
    }
}

fn adapter_data(state: &FunctionState) -> Result<&BindAdapterData> {
    icu_datefunc::bind_data(state.bind_data.as_ref())
}

/// `ICUDatePart::UnaryTimestampFunction`.
fn unary_timestamp_function<R: NativeType + Default>(
    args: &DataChunk,
    result: &mut Vector,
    info: &BindAdapterData,
    value: fn(Adapter, &mut Calendar, u64) -> Result<R>,
) -> Result<()> {
    let mut calendar: CalendarPtr = info.data.calendar.clone();
    let adapter = info
        .adapter
        .ok_or_else(|| Error::internal("ICU date part without an adapter"))?;
    unary::<Timestamp, R>(args, result, |input, out| {
        if input.is_finite() {
            let micros = set_time(&mut calendar, input);
            *out = value(adapter, &mut calendar, micros)?;
            Ok(true)
        } else {
            Ok(false)
        }
    })
}

fn bigint_value(adapter: Adapter, calendar: &mut Calendar, micros: u64) -> Result<i64> {
    match adapter {
        Adapter::Bigint(adapter) => adapter(calendar, micros),
        _ => Err(Error::internal("ICU BIGINT part with another adapter")),
    }
}

fn double_value(adapter: Adapter, calendar: &mut Calendar, micros: u64) -> Result<f64> {
    match adapter {
        Adapter::Double(adapter) => adapter(calendar, micros),
        _ => Err(Error::internal("ICU DOUBLE part with another adapter")),
    }
}

fn date_value(adapter: Adapter, calendar: &mut Calendar, micros: u64) -> Result<Date> {
    match adapter {
        Adapter::Date(adapter) => adapter(calendar, micros),
        _ => Err(Error::internal("ICU DATE part with another adapter")),
    }
}

fn unary_bigint(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    unary_timestamp_function::<i64>(args, result, adapter_data(state)?, bigint_value)
}

fn unary_double(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    unary_timestamp_function::<f64>(args, result, adapter_data(state)?, double_value)
}

fn unary_date(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    unary_timestamp_function::<Date>(args, result, adapter_data(state)?, date_value)
}

/// `UnaryTimestampFunction<timestamp_t, string_t>`.
fn unary_text(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let info = adapter_data(state)?;
    let mut calendar: CalendarPtr = info.data.calendar.clone();
    let Some(Adapter::Text(adapter)) = info.adapter else {
        return Err(Error::internal("ICU name part without a text adapter"));
    };
    unary::<Timestamp, StrOut>(args, result, |input, out| {
        if input.is_finite() {
            let micros = set_time(&mut calendar, input);
            adapter(&mut calendar, micros, out)?;
            Ok(true)
        } else {
            Ok(false)
        }
    })
}

/// `ICUDatePart::BindUnaryDatePart`: the adapter of the part the function is named after.
fn bind_unary_date_part(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let part = Part::parse(&function.name)?;
    let adapter = if part.is_bigint() {
        Adapter::Bigint(part_code_bigint_factory(part)?)
    } else {
        Adapter::Double(part_code_double_factory(part)?)
    };
    Ok(Some(Arc::new(BindAdapterData {
        data: BindData::from_client(input.client),
        adapter: Some(adapter),
    })))
}

/// `ICUDatePart::BinaryTimestampFunction`.
fn binary_timestamp_function(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let info = adapter_data(state)?;
    let mut calendar: CalendarPtr = info.data.calendar.clone();
    binary::<Str, Timestamp, i64>(args, result, |specifier, input, out| {
        if input.is_finite() {
            let micros = set_time(&mut calendar, input);
            let adapter = part_code_bigint_factory(Part::parse(&String::from_utf8_lossy(specifier))?)?;
            *out = adapter(&mut calendar, micros)?;
            Ok(true)
        } else {
            Ok(false)
        }
    })
}

/// The value column alone, for kernels that ignore a bound constant first argument.
fn second_column(args: &DataChunk) -> DataChunk {
    DataChunk::new(vec![args.column(1).clone()], args.len())
}

/// `ICUDatePart::BindBinaryDatePart`: if we are only looking for Julian Days (or epochs),
/// then patch in the unary function.
fn bind_binary_date_part(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    let part_value = input.constants.first().cloned().flatten();
    let is_parameter = input.argument_types[0].id() == LogicalTypeId::Unknown;
    if let Some(part_value) = part_value.filter(|value| !value.is_null() && !is_parameter) {
        let part_name = part_value.to_varchar()?;
        let part = Part::parse(&part_name)?;
        if !part.is_bigint() {
            function.return_type = LogicalType::DOUBLE;
            function.kernel =
                Arc::new(|args, state, result| unary_double(&second_column(args), state, result));
            let adapter = Adapter::Double(part_code_double_factory(part)?);
            return Ok(Some(Arc::new(BindAdapterData {
                data: BindData::from_client(input.client),
                adapter: Some(adapter),
            })));
        }
    }
    Ok(Some(Arc::new(BindAdapterData {
        data: BindData::from_client(input.client),
        adapter: None,
    })))
}

/// `ICUDatePart::BindStructData`.
#[derive(Debug)]
struct BindStructData {
    data: BindData,
    part_codes: Vec<Part>,
}

impl FunctionData for BindStructData {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn equals(&self, other: &dyn FunctionData) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self.data.equals(&other.data) && self.part_codes == other.part_codes)
    }
}

/// `ICUDatePart::SerializeStructFunction`.
fn serialize_struct(
    serializer: &mut dyn rustdb_serde::Serializer,
    bind_data: Option<&dyn FunctionData>,
    _: &ScalarFunction,
) -> Result<()> {
    let info = bind_data
        .and_then(|data| data.as_any().downcast_ref::<BindStructData>())
        .ok_or_else(|| Error::internal("date_part STRUCT variant without bind data"))?;
    serializer.write_property(100, "tz_setting", info.data.tz_setting.as_str())?;
    serializer.write_property(101, "cal_setting", info.data.cal_setting.as_str())?;
    let names: Vec<&str> = info.part_codes.iter().map(|part| part.specifier_name()).collect();
    serializer.write_property(102, "part_codes", &names)
}

/// `ICUDatePart::StructFunction`.
fn struct_function(args: &DataChunk, state: &mut FunctionState, result: &mut Vector) -> Result<()> {
    let info: &BindStructData = icu_datefunc::bind_data(state.bind_data.as_ref())?;
    let mut calendar: CalendarPtr = info.data.calendar.clone();
    let count = args.len();
    let input = addressable(args.column(0), count);
    let constant = input.is_constant();
    let rows = if constant { 1 } else { count };
    let values = input.unified::<Timestamp>();
    let parts = &info.part_codes;
    let mut bigints: Vec<Vec<Option<i64>>> = vec![Vec::with_capacity(rows); parts.len()];
    let mut doubles: Vec<Vec<Option<f64>>> = vec![Vec::with_capacity(rows); parts.len()];
    let mut nulls = Vec::new();
    for row in 0..rows {
        let Some(value) = values.get(row) else {
            nulls.push(row);
            for index in 0..parts.len() {
                bigints[index].push(None);
                doubles[index].push(None);
            }
            continue;
        };
        let micros = set_time(&mut calendar, value);
        let is_finite = value.is_finite();
        for (index, part) in parts.iter().enumerate() {
            if !is_finite {
                bigints[index].push(None);
                doubles[index].push(None);
            } else if part.is_bigint() {
                bigints[index].push(Some(part_code_bigint_factory(*part)?(&mut calendar, micros)?));
                doubles[index].push(None);
            } else {
                doubles[index].push(Some(part_code_double_factory(*part)?(&mut calendar, micros)?));
                bigints[index].push(None);
            }
        }
    }
    let mut fields = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        let field = if part.is_bigint() {
            column(LogicalType::BIGINT, &bigints[index])
        } else {
            column(LogicalType::DOUBLE, &doubles[index])
        };
        fields.push(field);
    }
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

/// A flat column of `values` (`None` is NULL).
fn column<T: NativeType>(ty: LogicalType, values: &[Option<T>]) -> Vector {
    let mut vector = Vector::flat(ty, values.len().max(1));
    vector.reset_flat(values.len());
    let mut nulls = Vec::new();
    {
        let data = vector.values_mut::<T>();
        for (row, value) in values.iter().enumerate() {
            match value {
                Some(value) => data[row] = *value,
                None => nulls.push(row),
            }
        }
    }
    let validity = vector.validity_mut();
    for row in nulls {
        validity.set_invalid(row, values.len());
    }
    vector
}

/// `ICUDatePart::BindStruct`: collect names and deconflict, construct return type.
fn bind_struct(
    function: &mut ScalarFunction,
    input: &BindInput<'_>,
) -> Result<Option<Arc<dyn FunctionData>>> {
    if input.argument_types[0].id() == LogicalTypeId::Unknown {
        return Err(rustdb_functions::parameter_not_resolved());
    }
    let not_constant = || {
        Error::binder(format!(
            "{} can only take constant lists of part names",
            function.name
        ))
    };
    let Some(Some(parts_list)) = input.constants.first() else {
        return Err(not_constant());
    };
    if parts_list.logical_type().id() != LogicalTypeId::List {
        return Err(not_constant());
    }
    let list_children = parts_list.children().unwrap_or_default();
    if list_children.is_empty() {
        return Err(Error::binder(format!(
            "{} requires non-empty lists of part names",
            function.name
        )));
    }
    let mut names: Vec<String> = Vec::with_capacity(list_children.len());
    let mut part_codes = Vec::with_capacity(list_children.len());
    let mut struct_children = Vec::with_capacity(list_children.len());
    for part_value in list_children {
        if part_value.is_null() {
            return Err(Error::binder(format!(
                "NULL struct entry name in {}",
                function.name
            )));
        }
        let part_name = part_value.to_varchar()?;
        let part_code = Part::parse(&part_name)?;
        if names.iter().any(|name| name.eq_ignore_ascii_case(&part_name)) {
            return Err(Error::binder(format!(
                "Duplicate struct entry name \"{part_name}\" in {}",
                function.name
            )));
        }
        names.push(part_name.clone());
        part_codes.push(part_code);
        let ty = if part_code.is_bigint() {
            LogicalType::BIGINT
        } else {
            LogicalType::DOUBLE
        };
        struct_children.push((part_name, ty));
    }
    // The part names are consumed (`EraseArgument`).
    function.erase_argument(0);
    function.return_type = LogicalType::struct_type(struct_children);
    Ok(Some(Arc::new(BindStructData {
        data: BindData::from_client(input.client),
        part_codes,
    })))
}

/// The bind of a unary function with a fixed adapter (`BindLastDate`, `BindMonthName`,
/// `BindDayName`).
fn bind_adapter(
    adapter: Adapter,
) -> impl Fn(&mut ScalarFunction, &BindInput<'_>) -> Result<Option<Arc<dyn FunctionData>>> {
    move |_, input| {
        Ok(Some(Arc::new(BindAdapterData {
            data: BindData::from_client(input.client),
            adapter: Some(adapter),
        })))
    }
}

/// `ICUDatePart::AddUnaryPartCodeFunctions`.
fn unary_part_set(name: &str, result_type: LogicalType) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    let kernel = if result_type == LogicalType::DOUBLE {
        unary_double
    } else {
        unary_bigint
    };
    set.add(
        ScalarFunction::new(name, vec![LogicalType::TIMESTAMP_TZ], result_type, kernel)
            .with_bind(bind_unary_date_part),
    );
    set
}

/// `ICUDatePart::AddDatePartFunctions`.
fn date_part_set(name: &str) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(
        ScalarFunction::new(
            name,
            vec![LogicalType::VARCHAR, LogicalType::TIMESTAMP_TZ],
            LogicalType::BIGINT,
            binary_timestamp_function,
        )
        .with_bind(bind_binary_date_part),
    );
    set.add(
        ScalarFunction::new(
            name,
            vec![LogicalType::list(LogicalType::VARCHAR), LogicalType::TIMESTAMP_TZ],
            LogicalType::struct_type(Vec::new()),
            struct_function,
        )
        .with_bind(bind_struct)
        .with_serialize(serialize_struct),
    );
    set
}

/// A unary function with a fixed adapter.
fn adapter_set(
    name: &str,
    result_type: LogicalType,
    kernel: fn(&DataChunk, &mut FunctionState, &mut Vector) -> Result<()>,
    adapter: Adapter,
) -> ScalarFunctionSet {
    let mut set = ScalarFunctionSet::new(name);
    set.add(
        ScalarFunction::new(name, vec![LogicalType::TIMESTAMP_TZ], result_type, kernel)
            .with_bind(bind_adapter(adapter)),
    );
    set
}

/// `RegisterICUDatePartFunctions`.
pub(crate) fn date_part_functions() -> Vec<ScalarFunctionSet> {
    let mut sets = Vec::new();
    // BIGINTs
    for name in [
        "era",
        "year",
        "month",
        "day",
        "decade",
        "century",
        "millennium",
        "microsecond",
        "millisecond",
        "second",
        "minute",
        "hour",
        "dayofweek",
        "isodow",
        "week", // Note that WeekOperator is ISO-8601, not US
        "dayofyear",
        "quarter",
        "isoyear",
        "timezone",
        "timezone_hour",
        "timezone_minute",
    ] {
        sets.push(unary_part_set(name, LogicalType::BIGINT));
    }
    // DOUBLEs
    sets.push(unary_part_set("epoch", LogicalType::DOUBLE));
    sets.push(unary_part_set("julian", LogicalType::DOUBLE));
    // register combinations
    sets.push(unary_part_set("yearweek", LogicalType::BIGINT)); // Note this is ISO year and week
    // register various aliases
    sets.push(unary_part_set("dayofmonth", LogicalType::BIGINT));
    sets.push(unary_part_set("weekday", LogicalType::BIGINT));
    sets.push(unary_part_set("weekofyear", LogicalType::BIGINT));
    // register the last_day function
    sets.push(adapter_set(
        "last_day",
        LogicalType::DATE,
        unary_date,
        Adapter::Date(make_last_day),
    ));
    // register the dayname/monthname functions
    sets.push(adapter_set(
        "monthname",
        LogicalType::VARCHAR,
        unary_text,
        Adapter::Text(month_name),
    ));
    sets.push(adapter_set(
        "dayname",
        LogicalType::VARCHAR,
        unary_text,
        Adapter::Text(day_name),
    ));
    // finally the actual date_part function
    sets.push(date_part_set("date_part"));
    sets.push(date_part_set("datepart"));
    sets
}
