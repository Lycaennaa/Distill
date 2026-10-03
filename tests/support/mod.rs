use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

pub fn fixture(name: &str) -> std::io::Result<PathBuf> {
    let root =
        std::env::temp_dir().join(format!("distill-phase3-cli-{name}-{}", std::process::id()));
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir_all(&root)?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"phase3-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    fs::write(root.join("Package.swift"), "// fixture package\n")?;
    fs::canonicalize(root)
}

pub fn run_distill(arguments: &[&str], path: &std::path::Path) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_distill"))
        .args(arguments)
        .env("PATH", path)
        .output()
}
