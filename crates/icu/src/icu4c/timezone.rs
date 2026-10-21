//! Port of the parts of ICU's `TimeZone`, `OlsonTimeZone` and `SimpleTimeZone` that the
//! calendars use: zone lookup by id (`TimeZone::createTimeZone`, custom `GMT±hh:mm` ids),
//! UTC and local offsets (`getOffset`, `BasicTimeZone::getOffsetFromLocal`), over the
//! tzdb that jiff bundles.

use std::sync::{Arc, OnceLock};

use super::grego::{ONE_HOUR, floor_divide_f64};
use super::zone_names::ZONE_NAMES;

/// Seconds of a 400-year Gregorian cycle (146097 days): offsets that follow a zone's final
/// rule repeat with this period.
const CYCLE_MILLIS: f64 = 146_097.0 * 86_400_000.0;
/// UTC instants after this one (9000-01-01) are moved back by whole cycles before jiff
/// looks them up; every zone follows its final rule by then.
const LATE_MILLIS: f64 = 221_845_392_000_000.0;
/// UTC instants before this one (-9000-01-01) have the zone's first (local mean time) offset.
const EARLY_MILLIS: f64 = -346_143_916_800_000.0;

/// A time zone (ICU's `TimeZone`).
#[derive(Clone, Debug)]
pub struct TimeZone {
    /// The id (`TimeZone::getID`).
    id: Arc<str>,
    kind: ZoneKind,
}

#[derive(Clone, Debug)]
enum ZoneKind {
    /// A zone of the tz database (`OlsonTimeZone`).
    Olson(jiff::tz::TimeZone),
    /// A fixed offset in milliseconds without DST (`SimpleTimeZone`, the custom
    /// `GMT±hh:mm` zones and `Etc/Unknown`).
    Fixed(i32),
}

impl PartialEq for TimeZone {
    /// `TimeZone::operator==`: same id and same rules.
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

/// `UNKNOWN_ZONE_ID`.
pub const UNKNOWN_ZONE_ID: &str = "Etc/Unknown";

/// The jiff zones of the ICU ids, loaded on first use.
fn zone_data(index: usize) -> Option<jiff::tz::TimeZone> {
    static ZONES: OnceLock<Vec<OnceLock<Option<jiff::tz::TimeZone>>>> = OnceLock::new();
    let zones = ZONES.get_or_init(|| ZONE_NAMES.iter().map(|_| OnceLock::new()).collect());
    zones
        .get(index)?
        .get_or_init(|| {
            let data = ZONE_NAMES[index].data;
            match data.strip_prefix("posix:") {
                Some(posix) => jiff::tz::TimeZone::posix(posix).ok(),
                None => jiff::tz::db().get(data).ok(),
            }
        })
        .clone()
}

/// The index of ICU zone id `id` (case-sensitive, like ICU's lookup in `Names`).
fn system_zone_index(id: &str) -> Option<usize> {
    ZONE_NAMES.binary_search_by(|zone| zone.name.cmp(id)).ok()
}

/// `TimeZone::parseCustomID`: `GMT[+-]hh[[:]mm[[:]ss]]`, as `(sign, hour, min, sec)`.
fn parse_custom_id(id: &str) -> Option<(i32, i32, i32, i32)> {
    const MAX_CUSTOM_HOUR: i32 = 23;
    const MAX_CUSTOM_MIN: i32 = 59;
    const MAX_CUSTOM_SEC: i32 = 59;
    let bytes = id.as_bytes();
    if bytes.len() <= 3 || !bytes[..3].eq_ignore_ascii_case(b"GMT") {
        return None;
    }
    let sign = match bytes[3] {
        b'-' => -1,
        b'+' => 1,
        _ => return None,
    };
    // `NumberFormat::parse` with integers only: a run of ASCII digits.
    let digits = |start: usize| -> (i64, usize) {
        let mut end = start;
        let mut value: i64 = 0;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            value = value.saturating_mul(10).saturating_add(i64::from(bytes[end] - b'0'));
            end += 1;
        }
        (value, end)
    };
    let start = 4;
    let (number, mut pos) = digits(start);
    if pos == start {
        return None;
    }
    let mut hour = i32::try_from(number).unwrap_or(i32::MAX);
    let mut min = 0;
    let mut sec = 0;
    if pos < bytes.len() {
        if pos - start > 2 || bytes[pos] != b':' {
            return None;
        }
        // hh:mm
        pos += 1;
        let old = pos;
        let (number, end) = digits(pos);
        if end - old != 2 {
            return None;
        }
        min = number as i32;
        pos = end;
        if pos < bytes.len() {
            if bytes[pos] != b':' {
                return None;
            }
            // [:ss]
            pos += 1;
            let old = pos;
            let (number, end) = digits(pos);
            if end != bytes.len() || end - old != 2 {
                return None;
            }
            sec = number as i32;
        }
    } else {
        let length = pos - start;
        if length == 0 || length > 6 {
            return None;
        }
        match length {
            3 | 4 => {
                min = hour % 100;
                hour /= 100;
            }
            5 | 6 => {
                sec = hour % 100;
                min = (hour / 100) % 100;
                hour /= 10000;
            }
            _ => {}
        }
    }
    if hour > MAX_CUSTOM_HOUR || min > MAX_CUSTOM_MIN || sec > MAX_CUSTOM_SEC {
        return None;
    }
    Some((sign, hour, min, sec))
}

/// `TimeZone::formatCustomID`.
fn format_custom_id(hour: i32, min: i32, sec: i32, negative: bool) -> String {
    let mut id = String::from("GMT");
    if hour | min | sec != 0 {
        id.push(if negative { '-' } else { '+' });
        id.push_str(&format!("{hour:02}:{min:02}"));
        if sec != 0 {
            id.push_str(&format!(":{sec:02}"));
        }
    }
    id
}

impl TimeZone {
    /// `TimeZone::createTimeZone`: a zone of the tz database, a custom `GMT±hh:mm` zone,
    /// or `Etc/Unknown`.
    pub fn create(id: &str) -> Self {
        Self::create_system(id)
            .or_else(|| Self::create_custom(id))
            .unwrap_or_else(Self::unknown)
    }

    /// `createSystemTimeZone`.
    fn create_system(id: &str) -> Option<Self> {
        let index = system_zone_index(id)?;
        let zone = zone_data(index)?;
        Some(Self {
            id: Arc::from(id),
            kind: ZoneKind::Olson(zone),
        })
    }

    /// `TimeZone::createCustomTimeZone`.
    fn create_custom(id: &str) -> Option<Self> {
        let (sign, hour, min, sec) = parse_custom_id(id)?;
        let custom_id = format_custom_id(hour, min, sec, sign < 0);
        let offset = sign * ((hour * 60 + min) * 60 + sec) * 1000;
        Some(Self {
            id: Arc::from(custom_id),
            kind: ZoneKind::Fixed(offset),
        })
    }

    /// `TimeZone::getUnknown`.
    pub fn unknown() -> Self {
        Self {
            id: Arc::from(UNKNOWN_ZONE_ID),
            kind: ZoneKind::Fixed(0),
        }
    }

    /// Whether this is `Etc/Unknown` (`*tz == TimeZone::getUnknown()`).
    pub fn is_unknown(&self) -> bool {
        &*self.id == UNKNOWN_ZONE_ID && matches!(self.kind, ZoneKind::Fixed(0))
    }

    /// `TimeZone::getID`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// `TimeZone::getOffset(date, local = FALSE, ...)`: the raw (standard) and DST offsets
    /// in milliseconds at UTC instant `date`.
    pub fn get_offset(&self, date: f64) -> (i32, i32) {
        match &self.kind {
            ZoneKind::Fixed(offset) => (*offset, 0),
            ZoneKind::Olson(zone) => {
                let date = normalize_instant(date);
                let millis = date as i64;
                let Ok(timestamp) = jiff::Timestamp::from_millisecond(millis) else {
                    return (0, 0);
                };
                let info = zone.to_offset_info(timestamp);
                split_offset(info.offset().seconds(), info.dst().is_dst())
            }
        }
    }

    /// `BasicTimeZone::getOffsetFromLocal(wall, kFormer, kLatter, ...)`: the offsets of
    /// local wall time `wall`. A wall time skipped by a transition takes the offsets before
    /// it; a repeated one takes those after it.
    pub fn get_offset_from_local(&self, wall: f64) -> (i32, i32) {
        match &self.kind {
            ZoneKind::Fixed(offset) => (*offset, 0),
            ZoneKind::Olson(zone) => {
                let wall = normalize_instant(wall);
                let Ok(timestamp) = jiff::Timestamp::from_millisecond(wall as i64) else {
                    return (0, 0);
                };
                let local = jiff::tz::Offset::UTC.to_datetime(timestamp);
                let ambiguous = zone.to_ambiguous_timestamp(local);
                let offset = match ambiguous.offset() {
                    jiff::tz::AmbiguousOffset::Unambiguous { offset } => offset,
                    jiff::tz::AmbiguousOffset::Gap { before, .. } => before,
                    jiff::tz::AmbiguousOffset::Fold { after, .. } => after,
                };
                // The DST flag of the offset in effect at the resolved instant.
                let instant = timestamp
                    .as_millisecond()
                    .saturating_sub(i64::from(offset.seconds()) * 1000);
                let dst = jiff::Timestamp::from_millisecond(instant)
                    .map(|instant| {
                        let info = zone.to_offset_info(instant);
                        info.offset() == offset && info.dst().is_dst()
                    })
                    .unwrap_or(false);
                split_offset(offset.seconds(), dst)
            }
        }
    }

    /// `TimeZone::getRawOffset` + DST at `date`: the total offset in milliseconds.
    pub fn total_offset(&self, date: f64) -> i32 {
        let (raw, dst) = self.get_offset(date);
        raw + dst
    }
}

/// Splits a total offset into ICU's raw and DST parts (the DST part is the usual hour).
fn split_offset(seconds: i32, dst: bool) -> (i32, i32) {
    let total = seconds * 1000;
    if dst { (total - ONE_HOUR, ONE_HOUR) } else { (total, 0) }
}

/// Moves `millis` into the range jiff covers without changing the offsets it has: late
/// instants back by whole 400-year cycles, early ones to the start of the range.
fn normalize_instant(millis: f64) -> f64 {
    if millis > LATE_MILLIS {
        let cycles = floor_divide_f64(millis - LATE_MILLIS, CYCLE_MILLIS) + 1.0;
        millis - cycles * CYCLE_MILLIS
    } else if millis < EARLY_MILLIS {
        EARLY_MILLIS
    } else {
        millis.floor()
    }
}

/// The host's time zone id (`TimeZone::detectHostTimeZone`): `TZ`, else the zone
/// `/etc/localtime` links to.
pub fn host_time_zone_id() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.strip_prefix(':').unwrap_or(&tz);
        if !tz.is_empty() {
            return tz.to_owned();
        }
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let target = target.to_string_lossy().into_owned();
        if let Some((_, zone)) = target.split_once("zoneinfo/") {
            return zone.to_owned();
        }
    }
    "Etc/Unknown".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_ids() {
        assert_eq!(TimeZone::create("GMT+5").id(), "GMT+05:00");
        assert_eq!(TimeZone::create("GMT-0530").id(), "GMT-05:30");
        assert_eq!(TimeZone::create("GMT+05:30:15").id(), "GMT+05:30:15");
        assert!(TimeZone::create("GMT+24").is_unknown());
        assert!(TimeZone::create("Fnord/Bar").is_unknown());
    }

    #[test]
    fn offsets() {
        let la = TimeZone::create("America/Los_Angeles");
        // 2021-07-01 00:00:00 UTC is PDT.
        assert_eq!(la.get_offset(1_625_097_600_000.0), (-8 * ONE_HOUR, ONE_HOUR));
        // 2021-03-14 02:30 local does not exist: the offsets before the transition apply.
        assert_eq!(la.get_offset_from_local(1_615_689_000_000.0), (-8 * ONE_HOUR, 0));
        // 2021-11-07 01:30 local happens twice: the later (standard) one applies.
        assert_eq!(la.get_offset_from_local(1_636_248_600_000.0), (-8 * ONE_HOUR, 0));
        // The final rule still applies in the far future.
        let summer_294000 = 9_215_000_000_000_000.0 - 150.0 * 86_400_000.0;
        assert_eq!(la.total_offset(summer_294000) % ONE_HOUR, 0);
        let tokyo = TimeZone::create("Asia/Tokyo");
        assert_eq!(tokyo.total_offset(-9_000_000_000_000_000.0), 33_539_000);
    }
}
