//! Records the version of the compiler, which `.version` shows like DuckDB's shell shows
//! the C++ compiler it was built with.

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let version = std::process::Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|text| text.split_whitespace().nth(1).map(str::to_owned))
        .unwrap_or_default();
    println!("cargo:rustc-env=RUSTDB_SHELL_RUSTC_VERSION={version}");
    println!("cargo:rerun-if-env-changed=RUSTC");
}
