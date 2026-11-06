//! Input order used when the C++ transports construct response headers.

use rustdb_common::{libcxx::ci_hash, platform_order::UnorderedOrder};

const STATUS: &str = "__RESPONSE_STATUS__";

pub(crate) fn input_order(headers: &[(String, Vec<u8>)], httplib: bool) -> Vec<usize> {
    if httplib {
        return httplib_order(headers);
    }
    // Curl's HeaderCollector starts each response with this private status entry.
    // TransformResponseCurl reinserts its iteration order into a second map,
    // excluding the marker. Its bucket history still influences that order.
    let mut order = UnorderedOrder::default();
    order.insert(ci_hash(STATUS), STATUS, |a, b| a.eq_ignore_ascii_case(b));
    let mut selected = vec![None];
    for (index, (name, _)) in headers.iter().enumerate() {
        let (inserted, _) = order.insert(ci_hash(name), name.as_str(), |a, b| a.eq_ignore_ascii_case(b));
        if inserted {
            selected.push(Some(index));
        }
    }
    order
        .order()
        .into_iter()
        .filter_map(|index| selected[index])
        .collect()
}

fn header_hash(name: &str) -> u64 {
    name.bytes().fold(0, |hash, byte| {
        ((u64::MAX >> 6) & hash.wrapping_mul(33)) ^ u64::from(byte.to_ascii_lowercase())
    })
}

fn next_prime(mut count: usize) -> usize {
    count = count.max(2);
    loop {
        let mut divisor = 2;
        let mut prime = true;
        while divisor <= count / divisor {
            if count.is_multiple_of(divisor) {
                prime = false;
                break;
            }
            divisor += 1;
        }
        if prime {
            return count;
        }
        count += 1;
    }
}

fn same(headers: &[(String, Vec<u8>)], a: usize, b: usize) -> bool {
    headers[a].0.eq_ignore_ascii_case(&headers[b].0)
}

#[cfg(not(target_os = "linux"))]
fn rehash(order: &mut Vec<usize>, hashes: &[u64], headers: &[(String, Vec<u8>)], buckets: usize) {
    if order.is_empty() {
        return;
    }
    let mut seen = vec![false; buckets];
    let mut previous = order[0];
    let mut previous_bucket = hashes[previous] as usize % buckets;
    seen[previous_bucket] = true;
    loop {
        let at = order
            .iter()
            .position(|&index| index == previous)
            .unwrap_or(order.len())
            + 1;
        if at >= order.len() {
            break;
        }
        let current = order[at];
        let bucket = hashes[current] as usize % buckets;
        if bucket == previous_bucket {
            previous = current;
        } else if seen[bucket] {
            // libc++ rehash_multi moves an entire equivalent-key group together.
            let mut end = at + 1;
            while end < order.len() && same(headers, current, order[end]) {
                end += 1;
            }
            let group: Vec<_> = order.drain(at..end).collect();
            let first = order
                .iter()
                .position(|&index| hashes[index] as usize % buckets == bucket)
                .unwrap_or(0);
            order.splice(first..first, group);
        } else {
            seen[bucket] = true;
            previous = current;
            previous_bucket = bucket;
        }
    }
}

#[cfg(target_os = "linux")]
fn rehash(order: &mut Vec<usize>, hashes: &[u64], headers: &[(String, Vec<u8>)], buckets: usize) {
    let old = std::mem::take(order);
    let mut start = 0;
    while start < old.len() {
        let mut end = start + 1;
        while end < old.len() && same(headers, old[start], old[end]) {
            end += 1;
        }
        let bucket = hashes[old[start]] as usize % buckets;
        let first = order
            .iter()
            .position(|&index| hashes[index] as usize % buckets == bucket)
            .unwrap_or(0);
        order.splice(first..first, old[start..end].iter().copied());
        start = end;
    }
}

fn httplib_order(headers: &[(String, Vec<u8>)]) -> Vec<usize> {
    let hashes: Vec<_> = headers.iter().map(|(name, _)| header_hash(name)).collect();
    let mut order = Vec::new();
    let mut buckets = 0;
    for (index, &hash) in hashes.iter().enumerate() {
        if order.len() + 1 > buckets {
            #[cfg(not(target_os = "linux"))]
            let requested =
                (2 * buckets + usize::from(!(buckets > 2 && buckets.is_power_of_two()))).max(order.len() + 1);
            #[cfg(target_os = "linux")]
            let requested = if buckets == 0 {
                12
            } else {
                (2 * buckets).max(order.len() + 1)
            };
            #[cfg(not(target_os = "linux"))]
            let count = if requested == 1 {
                2
            } else if requested.is_power_of_two() {
                requested
            } else {
                next_prime(requested)
            };
            #[cfg(target_os = "linux")]
            let count = next_prime(requested);
            rehash(&mut order, &hashes, headers, count);
            buckets = count;
        }
        let equal = order.iter().position(|&old| same(headers, old, index));
        let bucket = hash as usize % buckets;
        #[cfg(not(target_os = "linux"))]
        let at = if let Some(first) = equal {
            let mut end = first + 1;
            while end < order.len() && same(headers, order[first], order[end]) {
                end += 1;
            }
            end
        } else {
            order
                .iter()
                .rposition(|&old| hashes[old] as usize % buckets == bucket)
                .map_or(0, |at| at + 1)
        };
        #[cfg(target_os = "linux")]
        let at = equal.unwrap_or_else(|| {
            order
                .iter()
                .position(|&old| hashes[old] as usize % buckets == bucket)
                .unwrap_or(0)
        });
        order.insert(at, index);
    }
    order
}
