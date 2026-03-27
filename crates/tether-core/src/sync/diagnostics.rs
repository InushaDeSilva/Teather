//! Collect a ZIP diagnostics bundle (logs + DB path list) for support / parity testing.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use walkdir::WalkDir;
use zip::write::FileOptions;
use zip::ZipWriter;

/// Build a zip at `output_path` containing tether.db (if present) and small text files from `%LOCALAPPDATA%\Tether`.
pub fn collect_diagnostics_bundle(output_path: &Path) -> Result<PathBuf> {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    let tether_dir = PathBuf::from(local).join("Tether");

    let file = File::create(output_path).with_context(|| output_path.display().to_string())?;
    let mut zip = ZipWriter::new(file);
    let opts = FileOptions::default();

    let db = tether_dir.join("tether.db");
    if db.exists() {
        let data = std::fs::read(&db).with_context(|| db.display().to_string())?;
        zip.start_file("tether.db", opts)?;
        zip.write_all(&data)?;
    }

    let manifest = format!(
        "Tether diagnostics\nGenerated: {:?}\nTether dir: {}\n",
        std::time::SystemTime::now(),
        tether_dir.display()
    );
    zip.start_file("manifest.txt", opts)?;
    zip.write_all(manifest.as_bytes())?;

    let mut list = String::new();
    if tether_dir.exists() {
        for entry in WalkDir::new(&tether_dir).max_depth(3).into_iter().filter_map(|e| e.ok()) {
            list.push_str(&format!("{}\n", entry.path().display()));
        }
    } else {
        list.push_str("(Tether directory not found)\n");
    }
    zip.start_file("tree_listing.txt", opts)?;
    zip.write_all(list.as_bytes())?;

    zip.finish()?;
    Ok(output_path.to_path_buf())
}
