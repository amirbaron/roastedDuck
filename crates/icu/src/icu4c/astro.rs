//! ICU's `CalendarAstronomer` (`astro.cpp`): the sun and moon positions the astronomical
//! Islamic and the Chinese calendars are computed from. Only the members those calendars
//! use are ported, with ICU's operation order so the doubles round the same way.

#![allow(clippy::neg_cmp_op_on_partial_ord)] // ICU's loop conditions, NaN included

use super::grego;

/// `CalendarAstronomer::PI`.
const PI: f64 = std::f64::consts::PI;
/// `CalendarAstronomer_PI2`.
const PI2: f64 = PI * 2.0;
/// `CalendarAstronomer::SYNODIC_MONTH`: the average length of a lunation, in days.
pub(crate) const SYNODIC_MONTH: f64 = 29.530_588_853;
/// `TROPICAL_YEAR`: the average length of a solar year, in days.
const TROPICAL_YEAR: f64 = 365.242_191;
/// `MINUTE_MS`.
const MINUTE_MS: f64 = 60_000.0;
/// `DAY_MS`.
const DAY_MS: f64 = 86_400_000.0;
/// `JULIAN_EPOCH_MS`: the start of the Julian day numbering, in millis since 1970.
const JULIAN_EPOCH_MS: f64 = -210_866_760_000_000.0;
/// `JD_EPOCH`: the Julian day of the epoch of the orbital elements (1990).
const JD_EPOCH: f64 = 2_447_891.5;
/// `SUN_ETA_G`: the sun's ecliptic longitude at the epoch.
const SUN_ETA_G: f64 = 279.403_303 * PI / 180.0;
/// `SUN_OMEGA_G`: the sun's ecliptic longitude of perigee.
const SUN_OMEGA_G: f64 = 282.768_422 * PI / 180.0;
/// `SUN_E`: the eccentricity of the earth's orbit.
const SUN_E: f64 = 0.016_713;
/// `moonL0`: the moon's mean longitude at the epoch.
const MOON_L0: f64 = 318.351_648 * PI / 180.0;
/// `moonP0`: the moon's mean longitude of perigee.
const MOON_P0: f64 = 36.340_410 * PI / 180.0;
/// `moonN0`: the moon's mean longitude of the node.
const MOON_N0: f64 = 318.510_107 * PI / 180.0;
/// `moonI`: the inclination of the moon's orbit.
const MOON_I: f64 = 5.145_366 * PI / 180.0;

/// `normalize`.
fn normalize(value: f64, range: f64) -> f64 {
    value - range * grego::floor_divide_f64(value, range)
}

/// `norm2PI`.
fn norm_2pi(angle: f64) -> f64 {
    normalize(angle, PI * 2.0)
}

/// `normPI`.
fn norm_pi(angle: f64) -> f64 {
    normalize(angle + PI, PI * 2.0) - PI
}

/// `CalendarAstronomer::WINTER_SOLSTICE`.
pub(crate) const WINTER_SOLSTICE: f64 = (PI * 3.0) / 2.0;
/// `CalendarAstronomer::NEW_MOON`.
pub(crate) const NEW_MOON: f64 = 0.0;

/// `trueAnomaly`: solves Kepler's equation iteratively.
fn true_anomaly(mean_anomaly: f64, eccentricity: f64) -> f64 {
    let mut e = mean_anomaly;
    loop {
        let delta = e - eccentricity * e.sin() - mean_anomaly;
        e -= delta / (1.0 - eccentricity * e.cos());
        // `while (fabs(delta) > 1e-5)`, which also stops on NaN
        if !(delta.abs() > 1e-5) {
            break;
        }
    }
    2.0 * ((e / 2.0).tan() * ((1.0 + eccentricity) / (1.0 - eccentricity)).sqrt()).atan()
}

/// A `CalendarAstronomer` set to a time (`setTime`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Astronomer {
    time: f64,
}

impl Astronomer {
    /// `CalendarAstronomer(UDate)` / `setTime`.
    pub(crate) const fn new(time: f64) -> Self {
        Self { time }
    }

    /// `getJulianDay`.
    fn julian_day(self) -> f64 {
        (self.time - JULIAN_EPOCH_MS) / DAY_MS
    }

    /// `getSunLongitude(jDay, longitude, meanAnomaly)`: the sun's ecliptic longitude and
    /// mean anomaly.
    fn sun_longitude_and_anomaly(self) -> (f64, f64) {
        let day = self.julian_day() - JD_EPOCH; // Days since epoch

        // Find the angular distance the sun in a fictitious circular orbit has travelled
        // since the epoch.
        let epoch_angle = norm_2pi(PI2 / TROPICAL_YEAR * day);

        // The epoch wasn't at the sun's perigee; find the angular distance since perigee,
        // which is called the "mean anomaly"
        let mean_anomaly = norm_2pi(epoch_angle + SUN_ETA_G - SUN_OMEGA_G);

        // Now find the "true anomaly", e.g. the real solar longitude by solving Kepler's
        // equation for an elliptical orbit
        let longitude = norm_2pi(true_anomaly(mean_anomaly, SUN_E) + SUN_OMEGA_G);
        (longitude, mean_anomaly)
    }

    /// `getSunLongitude`: the sun's ecliptic longitude, in radians.
    pub(crate) fn sun_longitude(self) -> f64 {
        self.sun_longitude_and_anomaly().0
    }

    /// The `moonEclipLong` `getMoonPosition` computes, with the sun's longitude.
    fn moon_ecliptic_longitude(self) -> (f64, f64) {
        // Calculate the solar longitude.  Has the side effect of filling in
        // "meanAnomalySun" as well.
        let (sun_longitude, mean_anomaly_sun) = self.sun_longitude_and_anomaly();

        // Find the # of days since the epoch of our orbital parameters.
        let day = self.julian_day() - JD_EPOCH; // Days since epoch

        // Calculate the mean longitude and anomaly of the moon, based on a circular orbit.
        // Similar to the corresponding solar calculation.
        let mean_longitude = norm_2pi(13.176_396_6 * PI / 180.0 * day + MOON_L0);
        let mut mean_anomaly_moon = norm_2pi(mean_longitude - 0.111_404_1 * PI / 180.0 * day - MOON_P0);

        // Calculate the following corrections:
        //  Evection:   the sun's gravity affects the moon's eccentricity
        //  Annual Eqn: variation in the effect due to earth-sun distance
        //  A3:         correction factor (for ???)
        let evection =
            1.2739 * PI / 180.0 * (2.0 * (mean_longitude - sun_longitude) - mean_anomaly_moon).sin();
        let annual = 0.1858 * PI / 180.0 * mean_anomaly_sun.sin();
        let a3 = 0.3700 * PI / 180.0 * mean_anomaly_sun.sin();

        mean_anomaly_moon += evection - annual - a3;

        // More correction factors:
        //  center  equation of the center correction
        //  a4      yet another error correction (???)
        let center = 6.2886 * PI / 180.0 * mean_anomaly_moon.sin();
        let a4 = 0.2140 * PI / 180.0 * (2.0 * mean_anomaly_moon).sin();

        // Now find the moon's corrected longitude
        let mut moon_longitude = mean_longitude + evection + center - annual + a4;

        // And finally, find the variation, caused by the fact that the sun's gravitational
        // pull on the moon varies depending on which side of the earth the moon is on
        let variation = 0.6583 * PI / 180.0 * (2.0 * (moon_longitude - sun_longitude)).sin();

        moon_longitude += variation;

        // What we've calculated so far is the moon's longitude in the plane of its own
        // orbit.  Now map to the ecliptic to get the latitude and longitude.  First we
        // need to find the longitude of the ascending node, the position on the ecliptic
        // where it is crossed by the moon's orbit as it crosses from the southern to the
        // northern hemisphere.
        let mut node_longitude = norm_2pi(MOON_N0 - 0.052_953_9 * PI / 180.0 * day);

        node_longitude -= 0.16 * PI / 180.0 * mean_anomaly_sun.sin();

        let y = (moon_longitude - node_longitude).sin();
        let x = (moon_longitude - node_longitude).cos();

        let moon_eclip_long = (y * MOON_I.cos()).atan2(x) + node_longitude;
        (moon_eclip_long, sun_longitude)
    }

    /// `getMoonAge`: the angle between the moon and the sun, in radians.
    pub(crate) fn moon_age(self) -> f64 {
        let (moon_eclip_long, sun_longitude) = self.moon_ecliptic_longitude();
        norm_2pi(moon_eclip_long - sun_longitude)
    }

    /// `getSunTime`: the next or previous time the sun is at `desired` longitude.
    pub(crate) fn sun_time(&mut self, desired: f64, next: bool) -> f64 {
        self.time_of_angle(Self::sun_longitude, desired, TROPICAL_YEAR, MINUTE_MS, next)
    }

    /// `getMoonTime`: the next or previous time the moon is at `desired` age.
    pub(crate) fn moon_time(&mut self, desired: f64, next: bool) -> f64 {
        self.time_of_angle(Self::moon_age, desired, SYNODIC_MONTH, MINUTE_MS, next)
    }

    /// `timeOfAngle`: interpolates the time `func` reaches `desired`.
    fn time_of_angle(
        &mut self,
        func: fn(Self) -> f64,
        desired: f64,
        period_days: f64,
        epsilon: f64,
        next: bool,
    ) -> f64 {
        // Find the value of the function at the current time
        let mut last_angle = func(*self);

        // Find out how far we are from the desired angle
        let delta_angle = norm_2pi(desired - last_angle);

        // Using the average period, estimate the next (or previous) time at which the
        // desired angle occurs.
        let mut delta_t = (delta_angle + if next { 0.0 } else { -PI2 }) * (period_days * DAY_MS) / PI2;

        let mut last_delta_t = delta_t; // Liu
        let start_time = self.time; // Liu

        self.time += delta_t.ceil();

        // Now iterate until we get the error below epsilon.  Throughout this loop we use
        // normPI to get values in the range -Pi to Pi, since we're using them as
        // correction factors rather than absolute angles.
        loop {
            // Evaluate the function at the time we've estimated
            let angle = func(*self);

            // Find the # of milliseconds per radian at this point on the curve
            let factor = (delta_t / norm_pi(angle - last_angle)).abs();

            // Correct the time estimate based on how far off the angle is
            delta_t = norm_pi(desired - angle) * factor;

            // HACK: if abs(deltaT) begins to diverge we need to quit this loop: adjust our
            // start time by one eighth period days (either forward or backward) and try
            // again.
            if delta_t.abs() > last_delta_t.abs() {
                let delta = (period_days * DAY_MS / 8.0).ceil();
                self.time = start_time + if next { delta } else { -delta };
                return self.time_of_angle(func, desired, period_days, epsilon, next);
            }

            last_delta_t = delta_t;
            last_angle = angle;

            self.time += delta_t.ceil();
            // `while (fabs(deltaT) > epsilon)`, which also stops on NaN
            if !(delta_t.abs() > epsilon) {
                break;
            }
        }
        self.time
    }
}
