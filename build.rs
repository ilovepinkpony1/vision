//! Embeds the numbered SQL migrations of `src/ledger/migrations/`.
//!
//! A migration file is named `NNNN_words.sql`. The numbers must run 1, 2, 3, ... with no gap,
//! so that a later milestone adds a migration by adding one file, with no edit to a list.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const MIGRATIONS_DIR: &str = "src/ledger/migrations";

fn main() {
    println!("cargo:rerun-if-changed={MIGRATIONS_DIR}");
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let dir = manifest_dir.join(MIGRATIONS_DIR);
    let migrations = collect(&dir);
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("migrations.rs");
    fs::write(&out, render(&migrations)).expect("write the migration list");
}

fn collect(dir: &Path) -> Vec<(u32, String, PathBuf)> {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    let mut migrations = Vec::new();
    for entry in entries {
        let path = entry.expect("read a migration entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| panic!("migration name is not UTF-8: {}", path.display()))
            .to_string();
        let number = name
            .split('_')
            .next()
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or_else(|| {
                panic!("migration {name} must start with its number, such as 0002_words.sql")
            });
        migrations.push((number, name, path));
    }
    migrations.sort();
    for (index, (number, name, _)) in migrations.iter().enumerate() {
        let expected = index as u32 + 1;
        assert!(
            *number == expected,
            "migration {name} has number {number}, but the next number must be {expected}"
        );
    }
    migrations
}

fn render(migrations: &[(u32, String, PathBuf)]) -> String {
    let mut text = String::from("const MIGRATIONS: &[Migration] = &[\n");
    for (number, name, path) in migrations {
        let path = path.to_str().expect("migration path is UTF-8");
        text.push_str(&format!(
            "    Migration {{ version: {number}, name: {name:?}, sql: include_str!({path:?}) }},\n"
        ));
    }
    text.push_str("];\n");
    text
}
