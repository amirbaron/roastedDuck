//! The distributions of `tpcds.idx` (`dist.c`): weighted lists of values that the
//! generators draw from.
//!
//! The index is DuckDB's `tpcds_idx.hpp`, embedded as the file it was made from. Its
//! format: a big-endian entry count, then the distributions, then an index entry per
//! distribution at the end (`IDX_SIZE` bytes each).

use std::sync::OnceLock;

use super::decimal::atoi;
use super::genrand::{DIST_UNIFORM, Streams};

/// `tpcds.idx`.
static TPCDS_IDX: &[u8] = include_bytes!("tpcds.idx");

/// `D_NAME_LEN`.
const D_NAME_LEN: usize = 20;
/// `IDX_SIZE`: the name and 7 integers.
const IDX_SIZE: usize = D_NAME_LEN + 7 * 4;

/// `TKN_VARCHAR`.
const TKN_VARCHAR: i32 = 6;
/// `TKN_INT`.
const TKN_INT: i32 = 7;

/// A loaded distribution (`d_idx_t` with its `dist_t`).
#[derive(Debug)]
pub(crate) struct Dist {
    name: &'static str,
    /// The name in lower case, the key of `find_dist`'s search.
    key: String,
    length: usize,
    /// The type of each value set (`TKN_VARCHAR` or `TKN_INT`).
    types: Vec<i32>,
    /// The cumulative weights of each weight set.
    weights: Vec<Vec<i32>>,
    /// The total of each weight set.
    maximums: Vec<i32>,
    /// Whether the cumulative weights of each weight set never decrease: only `rowcounts`
    /// has negative weights.
    increasing: Vec<bool>,
    /// The values of each value set.
    values: Vec<Vec<&'static str>>,
    /// The values of each value set as `atoi` reads them.
    ints: Vec<Vec<i32>>,
}

impl Dist {
    /// `distsize`.
    pub(crate) fn size(&self) -> usize {
        self.length
    }

    /// `dist_max(dist, wset)`.
    pub(crate) fn max(&self, wset: usize) -> i32 {
        self.maximums[wset - 1]
    }

    /// The row a weighted random pick lands on, 0-based (`dist_op` with `op == 0`).
    #[inline]
    pub(crate) fn pick_index(&self, rng: &mut Streams, wset: usize, stream: usize) -> usize {
        let level = rng.genrand_integer(DIST_UNIFORM, 1, self.maximums[wset - 1], 0, stream);
        // `while (level > weight_sets[wset - 1][index] && index < length) index += 1;`, a
        // binary search where the cumulative weights never decrease.
        let weights = &self.weights[wset - 1];
        if self.increasing[wset - 1] {
            weights.partition_point(|&weight| level > weight)
        } else {
            weights
                .iter()
                .position(|&weight| level <= weight)
                .unwrap_or(weights.len())
        }
    }

    /// `pick_distribution` into a string: the value and its 1-based row.
    #[inline]
    pub(crate) fn pick_str(
        &self,
        rng: &mut Streams,
        vset: usize,
        wset: usize,
        stream: usize,
    ) -> (&'static str, i32) {
        let index = self.pick_index(rng, wset, stream);
        debug_assert_eq!(self.types[vset - 1], TKN_VARCHAR, "{}", self.name);
        (self.values[vset - 1][index], index as i32 + 1)
    }

    /// `pick_distribution` into an `int`: the value and its 1-based row.
    #[inline]
    pub(crate) fn pick_int(&self, rng: &mut Streams, vset: usize, wset: usize, stream: usize) -> (i32, i32) {
        let index = self.pick_index(rng, wset, stream);
        debug_assert_eq!(self.types[vset - 1], TKN_INT, "{}", self.name);
        (self.ints[vset - 1][index], index as i32 + 1)
    }

    /// The row index of `dist_member`, which exits on an over- or underrun.
    fn member_index(&self, row: i32) -> usize {
        match usize::try_from(row - 1) {
            Ok(index) if index < self.length => index,
            _ => unreachable!(
                "Distribution over-run/under-run in {}: index = {}, length={}",
                self.name,
                row - 1,
                self.length
            ),
        }
    }

    /// `dist_member` into a string: value set `vset` of 1-based row `row`.
    #[inline]
    pub(crate) fn member_str(&self, row: i32, vset: usize) -> &'static str {
        debug_assert_eq!(self.types[vset - 1], TKN_VARCHAR, "{}", self.name);
        self.values[vset - 1][self.member_index(row)]
    }

    /// `dist_member` into an `int`, or `dist_member(NULL, ...)` of an `int` set.
    #[inline]
    pub(crate) fn member_int(&self, row: i32, vset: usize) -> i32 {
        debug_assert_eq!(self.types[vset - 1], TKN_INT, "{}", self.name);
        self.ints[vset - 1][self.member_index(row)]
    }

    /// `dist_weight`: the weight of 1-based row `row` in weight set `wset`.
    #[inline]
    pub(crate) fn weight(&self, row: i32, wset: usize) -> i32 {
        let weights = &self.weights[wset - 1];
        let index = (row - 1) as usize;
        let mut res = weights[index];
        if index > 0 {
            res -= weights[index - 1];
        }
        res
    }
}

/// The distributions, sorted by name as `find_dist`'s `bsearch` wants them.
#[derive(Debug)]
pub(crate) struct Distributions {
    dists: Vec<Dist>,
}

/// A big-endian `int32_t` of the index at `offset`.
fn read_i32(offset: usize) -> i32 {
    let mut bytes = [0; 4];
    bytes.copy_from_slice(&TPCDS_IDX[offset..offset + 4]);
    i32::from_be_bytes(bytes)
}

/// The NUL-terminated string at the start of `bytes`.
fn c_str(bytes: &'static [u8]) -> &'static str {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).unwrap_or_default()
}

/// `load_dist`.
fn load_dist(
    name: &'static str,
    offset: usize,
    str_space: usize,
    length: usize,
    w_width: usize,
    v_width: usize,
    name_space: usize,
) -> Dist {
    let mut pos = offset;
    let mut next = || {
        let value = read_i32(pos);
        pos += 4;
        value
    };
    let types: Vec<i32> = (0..v_width).map(|_| next()).collect();
    // The weights, converted to cumulative ones.
    let mut maximums = Vec::with_capacity(w_width);
    let mut weights = Vec::with_capacity(w_width);
    for _ in 0..w_width {
        let mut total = 0;
        let set: Vec<i32> = (0..length)
            .map(|_| {
                total += next();
                total
            })
            .collect();
        maximums.push(total);
        weights.push(set);
    }
    let increasing = weights
        .iter()
        .map(|set: &Vec<i32>| set.windows(2).all(|pair| pair[0] <= pair[1]))
        .collect();
    let offsets: Vec<Vec<i32>> = (0..v_width)
        .map(|_| (0..length).map(|_| next()).collect())
        .collect();
    // The column aliases, then the values.
    let strings = &TPCDS_IDX[pos + name_space..pos + name_space + str_space];
    let values: Vec<Vec<&'static str>> = offsets
        .iter()
        .map(|set| {
            set.iter()
                .map(|&offset| c_str(&strings[offset as usize..]))
                .collect()
        })
        .collect();
    let ints = values
        .iter()
        .map(|set| set.iter().map(|value| atoi(value)).collect())
        .collect();
    Dist {
        name,
        key: name.to_ascii_lowercase(),
        length,
        types,
        weights,
        maximums,
        increasing,
        values,
        ints,
    }
}

impl Distributions {
    /// `find_dist`'s loading of the index, and `load_dist` of every distribution.
    fn load() -> Self {
        let entry_count = read_i32(0) as usize;
        let index = TPCDS_IDX.len() - entry_count * IDX_SIZE;
        let mut dists: Vec<Dist> = (0..entry_count)
            .map(|i| {
                let entry = index + i * IDX_SIZE;
                let name = c_str(&TPCDS_IDX[entry..entry + D_NAME_LEN]);
                let field = |n: usize| read_i32(entry + D_NAME_LEN + 4 * n) as usize;
                // index, offset, str_space, length, w_width, v_width, name_space
                load_dist(name, field(1), field(2), field(3), field(4), field(5), field(6))
            })
            .collect();
        // `qsort(..., di_compare)`: `strcasecmp` order.
        dists.sort_by(|a, b| a.key.cmp(&b.key));
        Self { dists }
    }

    /// `find_dist`: the distribution called `name`, ignoring case.
    pub(crate) fn get(&self, name: &str) -> &Dist {
        let name = name.to_ascii_lowercase();
        match self.dists.binary_search_by(|dist| dist.key.cmp(&name)) {
            Ok(index) => &self.dists[index],
            Err(_) => unreachable!("Invalid distribution name '{name}'"),
        }
    }
}

/// The distributions, loaded on first use.
pub(crate) fn distributions() -> &'static Distributions {
    static DISTRIBUTIONS: OnceLock<Distributions> = OnceLock::new();
    DISTRIBUTIONS.get_or_init(Distributions::load)
}

/// The distribution called `name`.
pub(crate) fn dist(name: &str) -> &'static Dist {
    distributions().get(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_loads() {
        let dists = distributions();
        assert_eq!(dists.dists.len(), 67);
        let calendar = dist("calendar");
        assert_eq!(calendar.size(), 366);
        assert_eq!(calendar.member_str(1, 2), "January");
        // Names are found ignoring case, as `web_site` asks for "Syllables".
        assert_eq!(dist("Syllables").size(), 10);
        // Picks search the cumulative weights, which only decrease in `rowcounts`, the one
        // distribution nothing picks from.
        for dist in &dists.dists {
            let increasing = dist.increasing.iter().all(|&increasing| increasing);
            assert_eq!(increasing, dist.name != "rowcounts", "{}", dist.name);
            assert!(dist.types.iter().all(|&ty| ty == TKN_INT || ty == TKN_VARCHAR));
        }
    }
}
