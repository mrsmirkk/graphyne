//! Filesystem helpers (blocking — call from background executors only).

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::path::Path;

/// Atomically replace `path` with `contents`: write a sibling temp file, fsync, rename.
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().context("file has no parent directory")?;
    let file_name = path.file_name().context("file has no name")?.to_string_lossy();
    let tmp = dir.join(format!(".{file_name}.graphyne-tmp"));
    {
        let mut f = std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

/// Heuristic binary sniff: a NUL byte or a high ratio of control chars in the first 8 KiB.
pub fn is_probably_binary(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut buf = [0u8; 8192];
    let Ok(n) = f.read(&mut buf) else { return false };
    let head = &buf[..n];
    if head.contains(&0) {
        return true;
    }
    if std::str::from_utf8(head).is_ok() {
        return false;
    }
    let control = head.iter().filter(|b| **b < 0x09 || (**b > 0x0D && **b < 0x20)).count();
    n > 0 && control * 10 > n
}

/// Read a text file, replacing invalid UTF-8.
pub fn read_text_lossy(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

/// Memory-map a file read-only (hex viewer).
pub fn map_file(path: &Path) -> Result<memmap2::Mmap> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    // SAFETY: the mapping is read-only; concurrent external truncation is the only hazard
    // and is accepted for an inspection view (memmap2 documents this contract).
    let map = unsafe { memmap2::Mmap::map(&f)? };
    Ok(map)
}

/// Write bytes in place at `offset` (hex editor).
pub fn patch_bytes(path: &Path, offset: u64, bytes: &[u8]) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::OpenOptions::new().write(true).open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}
