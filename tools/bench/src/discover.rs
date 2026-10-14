//! Finding benchmark files (`LoadInterpretedBenchmarks`) and selecting them by pattern.

use std::io;
use std::path::Path;

use regex::Regex;

/// Lists every `*.benchmark` file below `<root>/benchmark`, as sorted names relative to
/// `root` (`benchmark/micro/...`), which is how DuckDB's runner names benchmarks.
pub fn discover(root: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    walk(root, Path::new("benchmark"), &mut names)?;
    names.sort();
    Ok(names)
}

fn walk(root: &Path, relative: &Path, names: &mut Vec<String>) -> io::Result<()> {
    for entry in std::fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        // `metadata` follows symlinks, so linked benchmark directories are included.
        if std::fs::metadata(root.join(&path))?.is_dir() {
            walk(root, &path, names)?;
        } else if path.extension().is_some_and(|ext| ext == "benchmark") {
            names.push(path.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Include/exclude patterns: regular expressions searched anywhere in the benchmark name.
#[derive(Debug, Default)]
pub struct Selection {
    include: Vec<Regex>,
    exclude: Vec<Regex>,
}

impl Selection {
    /// Builds a selection; an empty `include` list selects everything.
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self, regex::Error> {
        let compile = |patterns: &[String]| {
            patterns
                .iter()
                .map(|p| Regex::new(p))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Self {
            include: compile(include)?,
            exclude: compile(exclude)?,
        })
    }

    /// Whether `name` is selected.
    pub fn matches(&self, name: &str) -> bool {
        (self.include.is_empty() || self.include.iter().any(|r| r.is_match(name)))
            && !self.exclude.iter().any(|r| r.is_match(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_searches_names() {
        let selection = Selection::new(
            &["tpch/sf1/".into(), "^benchmark/micro/agg".into()],
            &["q2[0-2]".into()],
        )
        .unwrap();
        assert!(selection.matches("benchmark/tpch/sf1/q01.benchmark"));
        assert!(!selection.matches("benchmark/tpch/sf1/q21.benchmark"));
        assert!(selection.matches("benchmark/micro/aggregate/x.benchmark"));
        assert!(!selection.matches("benchmark/tpch/sf1-parquet/q01.benchmark"));
        assert!(Selection::new(&[], &[]).unwrap().matches("anything"));
        assert!(Selection::new(&["(".into()], &[]).is_err());
    }
}
