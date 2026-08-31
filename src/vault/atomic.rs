//! Crash-safe file replacement.
//!
//! A half-written vault file is an unrecoverable wallet, so writes go to a
//! temporary file in the same directory, get fsynced, and are then renamed over
//! the target. The directory itself is fsynced so the rename survives a crash.

use anyhow::{Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .context("vault path has no parent directory")?;
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;

    let tmp = path.with_extension("tmp");
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }

    fs::rename(&tmp, path)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
