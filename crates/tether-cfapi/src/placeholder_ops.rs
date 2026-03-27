//! Free up space — dehydrate local payload back to a cloud placeholder.

use std::fs::File;
use std::path::Path;

use anyhow::Context;
use cloud_filter::ext::FileExt;

/// Remove local file bytes while keeping the placeholder (same as Explorer *Free up space*).
pub fn dehydrate_placeholder_file(path: &Path) -> anyhow::Result<()> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    f.dehydrate(..)
        .with_context(|| format!("CfDehydratePlaceholder for {}", path.display()))?;
    Ok(())
}
