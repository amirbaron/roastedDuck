//! Remote benchmark data. Several suites (h2oai, imdb, taxi, realnest, ...) read their
//! data over HTTP through the `httpfs` extension. The harness downloads those files once
//! (`bench fetch`) and rewrites the URLs in the benchmark's SQL to the local copies, so
//! that neither engine needs network access or `httpfs` while being measured.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

use crate::format::Benchmark;

/// A URL inside a single-quoted SQL string literal.
static URL_LITERAL: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"'(https?://[^'\s]+)'").ok());

/// The extension made unnecessary by mirroring.
const HTTP_EXTENSION: &str = "httpfs";

/// Every remote URL referenced by the SQL sections of `benchmark`.
pub fn urls(benchmark: &Benchmark) -> Vec<String> {
    let mut urls: Vec<String> = sections(benchmark).flat_map(find_urls).collect();
    urls.sort();
    urls.dedup();
    urls
}

fn sections(benchmark: &Benchmark) -> impl Iterator<Item = &str> {
    let q = &benchmark.queries;
    [&q.load, &q.reload, &q.init, &q.cleanup]
        .into_iter()
        .filter_map(Option::as_deref)
        .chain(std::iter::once(q.run.as_str()))
}

fn find_urls(sql: &str) -> Vec<String> {
    URL_LITERAL.as_ref().map_or_else(Vec::new, |re| {
        re.captures_iter(sql).map(|c| c[1].to_owned()).collect()
    })
}

/// Where the mirror of `url` lives below `downloads`: `<host>/<path>`.
pub fn local_path(downloads: &Path, url: &str) -> PathBuf {
    let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let without_query = without_scheme.split(['?', '#']).next().unwrap_or(without_scheme);
    without_query
        .split('/')
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
        .fold(downloads.to_path_buf(), |path, part| path.join(part))
}

/// The result of [`localize`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Localized {
    /// URLs replaced by local paths.
    pub replaced: usize,
    /// URLs without a local copy (the benchmark still needs the network).
    pub missing: Vec<String>,
}

/// Rewrites every URL with a local copy to that copy's absolute path. When no URL is
/// left, `httpfs` is dropped from the benchmark's requirements.
pub fn localize(benchmark: &mut Benchmark, downloads: &Path) -> Localized {
    let mut localized = Localized::default();
    for url in urls(benchmark) {
        let local = local_path(downloads, &url);
        if !local.is_file() {
            localized.missing.push(url);
            continue;
        }
        let from = format!("'{url}'");
        let to = format!("'{}'", local.display());
        let q = &mut benchmark.queries;
        for sql in [&mut q.load, &mut q.reload, &mut q.init, &mut q.cleanup]
            .into_iter()
            .flatten()
            .chain(std::iter::once(&mut q.run))
        {
            *sql = sql.replace(&from, &to);
        }
        localized.replaced += 1;
    }
    if localized.missing.is_empty() {
        benchmark.extensions.retain(|e| e != HTTP_EXTENSION);
        benchmark.load_extensions.retain(|e| e != HTTP_EXTENSION);
    }
    localized
}

/// Whether the benchmark (after [`localize`]) still needs `httpfs`.
pub fn needs_network(benchmark: &Benchmark) -> bool {
    benchmark
        .extensions
        .iter()
        .chain(&benchmark.load_extensions)
        .any(|e| e == HTTP_EXTENSION)
        || !urls(benchmark).is_empty()
}

/// Downloads `url` to its mirror path with `curl` (resuming partial downloads).
pub fn download(downloads: &Path, url: &str) -> std::io::Result<PathBuf> {
    let target = local_path(downloads, url);
    if target.is_file() {
        return Ok(target);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut partial = target.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let status = Command::new("curl")
        .args([
            "--location",
            "--fail",
            "--silent",
            "--show-error",
            "--retry",
            "3",
            "--continue-at",
            "-",
        ])
        .arg("--output")
        .arg(&partial)
        .arg(url)
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!("curl failed for {url} ({status})")));
    }
    std::fs::rename(&partial, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Queries;

    fn benchmark(load: &str) -> Benchmark {
        Benchmark {
            name: "benchmark/h2oai/q01.benchmark".into(),
            display_name: String::new(),
            group: String::new(),
            subgroup: String::new(),
            description: None,
            queries: Queries {
                load: Some(load.into()),
                run: "SELECT 1".into(),
                ..Queries::default()
            },
            extensions: vec![],
            load_extensions: vec!["httpfs".into()],
            cache: crate::format::Cache::default(),
            storage: crate::format::Storage::default(),
            require_reinit: false,
            result_mode: crate::format::ResultMode::Materialized,
            retry_load: 0,
            asserts: vec![],
            result: None,
            arguments: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn finds_and_maps_urls() {
        let b = benchmark(
            "CREATE TABLE x AS FROM read_csv_auto('https://h.org/a/b.csv.gz'); SELECT 'http://x.y/z?q=1'",
        );
        assert_eq!(urls(&b), vec!["http://x.y/z?q=1", "https://h.org/a/b.csv.gz"]);
        assert_eq!(
            local_path(Path::new("/d"), "https://h.org/a/../b.csv.gz"),
            PathBuf::from("/d/h.org/a/b.csv.gz")
        );
        assert_eq!(
            local_path(Path::new("/d"), "http://x.y/z?q=1"),
            PathBuf::from("/d/x.y/z")
        );
    }

    #[test]
    fn localizes_available_files_and_drops_httpfs() {
        let dir = std::env::temp_dir().join(format!("roastedduck-bench-data-{}", std::process::id()));
        let url = "https://h.org/a/b.csv.gz";
        let local = local_path(&dir, url);
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::fs::write(&local, "x").unwrap();

        let mut b = benchmark(&format!("FROM read_csv('{url}')"));
        assert!(needs_network(&b));
        let localized = localize(&mut b, &dir);
        assert_eq!(
            localized,
            Localized {
                replaced: 1,
                missing: vec![]
            }
        );
        assert_eq!(
            b.queries.load.as_deref(),
            Some(format!("FROM read_csv('{}')", local.display()).as_str())
        );
        assert!(!needs_network(&b));

        let mut b = benchmark("FROM read_csv('https://h.org/missing.csv')");
        let localized = localize(&mut b, &dir);
        assert_eq!(localized.missing, vec!["https://h.org/missing.csv"]);
        assert!(needs_network(&b));
        let _ = std::fs::remove_dir_all(dir);
    }
}
