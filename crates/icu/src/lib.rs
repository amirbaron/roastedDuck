//! The ICU extension (DuckDB's `extension/icu`): the `TimeZone` and `Calendar` settings,
//! calendar-aware TIMESTAMP WITH TIME ZONE functions and casts, and ICU collations.
//!
//! ICU's calendars and time zones are ported in [`icu4c`] (over the tz database jiff
//! bundles); the `icu_*` modules port the extension's sources file by file. The database
//! registers what [`IcuExtension`] lists when the extension is loaded
//! (`IcuExtension::Load`).

pub mod icu4c;
mod icu_collation;
mod icu_current;
mod icu_dateadd;
mod icu_datefunc;
mod icu_datepart;
mod icu_datesub;
mod icu_datetrunc;
mod icu_extension;
mod icu_helpers;
mod icu_list_range;
mod icu_makedate;
mod icu_strptime;
mod icu_table_range;
mod icu_timebucket;
mod icu_timezone;

use rustdb_common::Result;
use rustdb_function::cast::CastFunctionSet;
use rustdb_function::{ExtensionOption, ScalarFunctionSet, TableFunctionSet};

/// The extension's name (`IcuExtension::Name`).
pub const EXTENSION_NAME: &str = "icu";

/// The functions whose existing overloads the extension patches (`ICUStrptime::TailPatch`).
pub const PATCHED_FUNCTIONS: [&str; 2] = ["strptime", "try_strptime"];

/// What loading the extension registers (`LoadInternal`).
#[derive(Debug)]
pub struct IcuExtension;

impl IcuExtension {
    /// The scalar function sets, in registration order. Sets whose name exists already
    /// add their overloads to it, replacing overloads with the same signature
    /// (`ExtensionLoader::RegisterFunction` with `ALTER_ON_CONFLICT`).
    pub fn scalar_functions() -> Vec<ScalarFunctionSet> {
        let mut sets = vec![icu_collation::icu_sort_key_functions()];
        sets.extend(icu_current::current_functions());
        sets.extend(icu_dateadd::date_add_functions());
        sets.extend(icu_datepart::date_part_functions());
        sets.extend(icu_datesub::date_sub_functions());
        sets.extend(icu_datetrunc::date_trunc_functions());
        sets.push(icu_makedate::make_timestamptz_functions());
        sets.extend(icu_list_range::list_range_functions());
        sets.push(icu_strptime::strftime_functions());
        sets.push(icu_timebucket::time_bucket_functions());
        sets.push(icu_timezone::timezone_functions());
        sets.extend(icu_timezone::local_now_functions());
        sets
    }

    /// The table function sets, in registration order (added to existing sets like the
    /// scalar functions).
    pub fn table_functions() -> Vec<TableFunctionSet> {
        let mut sets = icu_table_range::table_range_functions();
        sets.push(icu_timezone::pg_timezone_names());
        sets.push(icu_extension::icu_calendar_names());
        sets
    }

    /// Patches the existing overloads of one of [`PATCHED_FUNCTIONS`].
    pub fn patch_function(set: &mut ScalarFunctionSet) -> Result<()> {
        icu_strptime::tail_patch_strptime(set)
    }

    /// Registers the extension's casts (`RegisterCastFunction`).
    pub fn register_casts(casts: &mut CastFunctionSet) {
        icu_makedate::add_casts(casts);
        icu_strptime::add_casts(casts);
        icu_timezone::add_casts(casts);
    }

    /// The collations, by name, with the function computing a string's collation key
    /// (`RegisterCollation`).
    pub fn collations() -> Vec<(String, rustdb_function::ScalarFunction)> {
        icu_collation::collations()
    }

    /// The extension's configuration options (`AddExtensionOption`).
    pub fn options() -> Vec<ExtensionOption> {
        icu_extension::extension_options()
    }
}
