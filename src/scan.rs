//! Walks a folder and picks out the files that could have duplicates.

use crate::progress;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

pub struct FileInfo {
    pub path: PathBuf,
    pub size: u64,
    /// Nanoseconds since the Unix epoch, if the filesystem provides it.
    pub mtime: Option<u128>,
}

#[derive(Default)]
pub struct ScanStats {
    pub scanned: usize,
    pub unique: usize,
    pub empty: usize,
    pub unreadable: usize,
}

impl fmt::Display for ScanStats {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let ScanStats {
            scanned,
            unique,
            empty,
            unreadable,
        } = self;
        write!(
            f,
            "Scanned {scanned} files: {unique} unique size, {empty} empty"
        )?;
        if *unreadable > 0 {
            write!(f, ", {unreadable} unreadable")?;
        }
        Ok(())
    }
}

pub struct Scan {
    /// Files that share their size with at least one other file.
    pub candidates: Vec<FileInfo>,
    pub stats: ScanStats,
}

/// Recursively scans `root`, skipping `exclude` and not following symlinks.
///
/// A file with a unique size can't have a duplicate, so only files sharing a
/// size are returned as candidates; this avoids hashing most of the tree.
/// Empty files are all "identical", so they're skipped rather than wiping out
/// placeholders like `.gitkeep`.
pub fn scan(root: &Path, exclude: Option<&Path>) -> Scan {
    let mut by_size: HashMap<u64, Vec<FileInfo>> = HashMap::new();
    let mut stats = ScanStats::default();
    let bar = progress::spinner("{spinner} Scanning: {pos} files found");

    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !exclude.is_some_and(|x| e.path().starts_with(x)));
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                progress::warn(&bar, e);
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        stats.scanned += 1;
        bar.inc(1);
        match entry.metadata() {
            Ok(m) if m.len() > 0 => {
                let mtime = mtime(&m);
                let file = FileInfo {
                    path: entry.into_path(),
                    size: m.len(),
                    mtime,
                };
                by_size.entry(file.size).or_default().push(file);
            }
            Ok(_) => stats.empty += 1,
            Err(e) => {
                stats.unreadable += 1;
                progress::warn(&bar, e);
            }
        }
    }
    bar.finish_and_clear();

    stats.unique = by_size.values().filter(|files| files.len() == 1).count();
    let candidates = by_size
        .into_values()
        .filter(|files| files.len() > 1)
        .flatten()
        .collect();
    Scan { candidates, stats }
}

fn mtime(meta: &fs::Metadata) -> Option<u128> {
    let d = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(d.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn candidate_names(scan: &Scan, root: &Path) -> Vec<String> {
        let mut names: Vec<String> = scan
            .candidates
            .iter()
            .map(|f| {
                f.path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn only_files_sharing_a_size_are_candidates() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(root, "a.txt", "four");
        write(root, "sub/b.txt", "also"); // same size as a.txt, different content
        write(root, "unique.txt", "a different size");
        write(root, "empty1", "");
        write(root, "sub/empty2", "");

        let scan = scan(root, None);

        assert_eq!(candidate_names(&scan, root), ["a.txt", "sub/b.txt"]);
        let s = &scan.stats;
        assert_eq!((s.scanned, s.unique, s.empty, s.unreadable), (5, 1, 2, 0));
        assert!(scan
            .candidates
            .iter()
            .all(|f| f.size == 4 && f.mtime.is_some()));
    }

    #[test]
    fn excluded_folder_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(root, "a", "same");
        write(root, "b", "same");
        write(root, "dest/c", "same");

        let scan = scan(root, Some(&root.join("dest")));

        assert_eq!(candidate_names(&scan, root), ["a", "b"]);
        assert_eq!(scan.stats.scanned, 2);
    }

    #[test]
    fn symlinks_are_ignored() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write(root, "a", "same");
        write(root, "b", "same");
        std::os::unix::fs::symlink(root.join("a"), root.join("link")).unwrap();

        let scan = scan(root, None);

        assert_eq!(candidate_names(&scan, root), ["a", "b"]);
        assert_eq!(scan.stats.scanned, 2);
    }

    #[test]
    fn stats_display_shows_unreadable_only_when_nonzero() {
        let mut stats = ScanStats {
            scanned: 10,
            unique: 4,
            empty: 1,
            unreadable: 0,
        };
        assert_eq!(
            stats.to_string(),
            "Scanned 10 files: 4 unique size, 1 empty"
        );
        stats.unreadable = 2;
        assert_eq!(
            stats.to_string(),
            "Scanned 10 files: 4 unique size, 1 empty, 2 unreadable"
        );
    }
}
