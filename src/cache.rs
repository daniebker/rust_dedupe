//! Persistent hash cache, so a repeat run (e.g. the real run after a dry run)
//! only rehashes files whose size or modification time has changed.
//!
//! Stored as tab-separated lines: `hash  size  mtime_nanos  path`. The path is
//! last so it may contain tabs; paths that aren't UTF-8 or contain newlines,
//! and files without a modification time, are simply not cached.
//!
//! During a run, each new hash is appended to the file as soon as it is
//! computed, so an interrupted run loses no finished work. Later lines
//! override earlier ones, and the file is rewritten compactly at the end.

use crate::hash::{Hash, HashedFile};
use crate::scan::FileInfo;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const HEADER: &str = "dedupe-cache v1";

#[derive(Clone, Copy)]
struct Entry {
    size: u64,
    mtime: u128,
    hash: Hash,
}

impl Entry {
    fn of(h: &HashedFile) -> Option<Self> {
        Some(Entry {
            size: h.file.size,
            mtime: h.file.mtime?,
            hash: h.hash,
        })
    }
}

/// The hash cache for one run. If the cache file can't be located, read or
/// written, it degrades to a cache that remembers nothing, so callers never
/// need to check whether caching is available.
pub struct HashCache {
    file: Option<PathBuf>,
    entries: HashMap<PathBuf, Entry>,
    journal: Option<Mutex<File>>,
}

impl HashCache {
    /// Loads the cache from `$XDG_CACHE_HOME/dedupe/hashes.tsv` (falling back
    /// to `~/.cache`), rewrites it compactly and opens it for appending.
    pub fn open() -> Self {
        Self::open_at(location())
    }

    /// Like [`open`](HashCache::open) but with an explicit cache file; `None`
    /// gives a cache that remembers nothing.
    fn open_at(file: Option<PathBuf>) -> Self {
        let Some(file) = file else {
            return HashCache {
                file: None,
                entries: HashMap::new(),
                journal: None,
            };
        };
        let entries = load(&file);
        let journal = write_all(&file, &entries)
            .and_then(|()| OpenOptions::new().append(true).open(&file))
            .map_err(|e| warn_unavailable(&file, &e))
            .ok()
            .map(Mutex::new);
        HashCache {
            file: Some(file),
            entries,
            journal,
        }
    }

    /// Splits `files` into those with a still-valid cached hash and the rest.
    pub fn split_cached(&self, files: Vec<FileInfo>) -> (Vec<HashedFile>, Vec<FileInfo>) {
        let mut cached = Vec::new();
        let mut uncached = Vec::new();
        for file in files {
            match self.lookup(&file) {
                Some(hash) => cached.push(HashedFile { file, hash }),
                None => uncached.push(file),
            }
        }
        (cached, uncached)
    }

    fn lookup(&self, file: &FileInfo) -> Option<Hash> {
        let e = self.entries.get(&file.path)?;
        (e.size == file.size && Some(e.mtime) == file.mtime).then_some(e.hash)
    }

    /// Appends one new hash to the cache file straight away. Unbuffered, so it
    /// survives the process being killed; best effort, since [`save`] writes
    /// everything again at the end.
    ///
    /// [`save`]: HashCache::save
    pub fn record(&self, hashed: &HashedFile) {
        let Some(journal) = &self.journal else { return };
        if let Some(line) = Entry::of(hashed).and_then(|e| format_line(&hashed.file.path, &e)) {
            let _ = journal.lock().unwrap().write_all(line.as_bytes());
        }
    }

    /// Replaces the cached entries under `root` with `hashed`, keeping other
    /// folders' entries, and rewrites the cache file.
    pub fn save(mut self, root: &Path, hashed: &[HashedFile]) {
        // Close the append handle before the file is replaced underneath it.
        self.journal = None;
        let Some(file) = &self.file else { return };
        self.entries.retain(|p, _| !p.starts_with(root));
        for h in hashed {
            if let Some(e) = Entry::of(h) {
                self.entries.insert(h.file.path.clone(), e);
            }
        }
        if let Err(e) = write_all(file, &self.entries) {
            eprintln!("warning: could not save hash cache {}: {e}", file.display());
        }
    }
}

fn location() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("dedupe").join("hashes.tsv"))
}

fn warn_unavailable(file: &Path, e: &io::Error) {
    eprintln!("warning: hash cache {} unavailable: {e}", file.display());
}

/// A missing, unreadable or outdated file yields an empty cache.
fn load(file: &Path) -> HashMap<PathBuf, Entry> {
    let Ok(text) = fs::read_to_string(file) else {
        return HashMap::new();
    };
    // A run killed mid-append can leave a partial last line. A truncated path
    // could name a different real file, so only trust newline-terminated lines.
    let complete = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
    let mut lines = complete.lines();
    if lines.next() != Some(HEADER) {
        return HashMap::new();
    }
    lines.filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<(PathBuf, Entry)> {
    let mut fields = line.splitn(4, '\t');
    let hash = *blake3::Hash::from_hex(fields.next()?).ok()?.as_bytes();
    let size = fields.next()?.parse().ok()?;
    let mtime = fields.next()?.parse().ok()?;
    let path = PathBuf::from(fields.next()?);
    Some((path, Entry { size, mtime, hash }))
}

/// One newline-terminated cache line, or None if the path can't be stored.
fn format_line(path: &Path, e: &Entry) -> Option<String> {
    let Entry { size, mtime, hash } = *e;
    let p = path.to_str().filter(|p| !p.contains('\n'))?;
    let hash = blake3::Hash::from(hash).to_hex();
    Some(format!("{hash}\t{size}\t{mtime}\t{p}\n"))
}

/// Writes to a temp file and renames it into place, so an interrupted write
/// never leaves a truncated cache.
fn write_all(file: &Path, entries: &HashMap<PathBuf, Entry>) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("tsv.tmp");
    let mut out = BufWriter::new(File::create(&tmp)?);
    writeln!(out, "{HEADER}")?;
    for (path, e) in entries {
        if let Some(line) = format_line(path, e) {
            out.write_all(line.as_bytes())?;
        }
    }
    out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn hashed(path: &str, size: u64, mtime: Option<u128>, hash_byte: u8) -> HashedFile {
        let file = FileInfo {
            path: path.into(),
            size,
            mtime,
        };
        HashedFile {
            file,
            hash: [hash_byte; 32],
        }
    }

    fn info(path: &str, size: u64, mtime: u128) -> FileInfo {
        FileInfo {
            path: path.into(),
            size,
            mtime: Some(mtime),
        }
    }

    fn cache_file(tmp: &TempDir) -> PathBuf {
        tmp.path().join("sub/hashes.tsv")
    }

    fn open(tmp: &TempDir) -> HashCache {
        HashCache::open_at(Some(cache_file(tmp)))
    }

    fn cached_paths(cache: &HashCache, files: Vec<FileInfo>) -> Vec<PathBuf> {
        let (cached, _) = cache.split_cached(files);
        cached.into_iter().map(|h| h.file.path).collect()
    }

    #[test]
    fn saved_hashes_are_reused_by_the_next_run() {
        let tmp = TempDir::new().unwrap();
        open(&tmp).save(Path::new("/r"), &[hashed("/r/a", 5, Some(100), 9)]);

        let (cached, uncached) = open(&tmp).split_cached(vec![info("/r/a", 5, 100)]);

        assert!(uncached.is_empty());
        assert_eq!(cached[0].hash, [9; 32]);
    }

    #[test]
    fn changed_size_or_mtime_invalidates_entry() {
        let tmp = TempDir::new().unwrap();
        open(&tmp).save(Path::new("/r"), &[hashed("/r/a", 5, Some(100), 9)]);
        let cache = open(&tmp);

        assert!(cached_paths(&cache, vec![info("/r/a", 6, 100)]).is_empty());
        assert!(cached_paths(&cache, vec![info("/r/a", 5, 101)]).is_empty());
        let no_mtime = FileInfo {
            path: "/r/a".into(),
            size: 5,
            mtime: None,
        };
        assert!(cached_paths(&cache, vec![no_mtime]).is_empty());
    }

    #[test]
    fn recorded_hashes_survive_without_a_final_save() {
        let tmp = TempDir::new().unwrap();
        let cache = open(&tmp);
        cache.record(&hashed("/r/a", 5, Some(100), 9));
        drop(cache); // Simulates the run being interrupted before save().

        assert_eq!(
            cached_paths(&open(&tmp), vec![info("/r/a", 5, 100)]),
            [PathBuf::from("/r/a")]
        );
    }

    #[test]
    fn later_records_override_earlier_ones() {
        let tmp = TempDir::new().unwrap();
        let cache = open(&tmp);
        cache.record(&hashed("/r/a", 5, Some(100), 1));
        cache.record(&hashed("/r/a", 5, Some(200), 2));
        drop(cache);

        let (cached, _) = open(&tmp).split_cached(vec![info("/r/a", 5, 200)]);
        assert_eq!(cached[0].hash, [2; 32]);
    }

    #[test]
    fn save_replaces_entries_under_root_and_keeps_others() {
        let tmp = TempDir::new().unwrap();
        open(&tmp).save(
            Path::new("/"),
            &[
                hashed("/r/old", 1, Some(1), 1),
                hashed("/other/x", 1, Some(1), 1),
            ],
        );
        open(&tmp).save(Path::new("/r"), &[hashed("/r/new", 1, Some(1), 1)]);

        let files = vec![
            info("/r/old", 1, 1),
            info("/r/new", 1, 1),
            info("/other/x", 1, 1),
        ];
        let mut kept = cached_paths(&open(&tmp), files);
        kept.sort();
        assert_eq!(kept, [PathBuf::from("/other/x"), PathBuf::from("/r/new")]);
    }

    #[test]
    fn truncated_last_line_is_ignored() {
        let tmp = TempDir::new().unwrap();
        open(&tmp).save(Path::new("/r"), &[hashed("/r/abc", 5, Some(100), 9)]);
        // Simulate a kill mid-write: a valid-looking line for "/r/ab" with no newline.
        let file = cache_file(&tmp);
        let mut text = fs::read_to_string(&file).unwrap();
        let line = text.lines().nth(1).unwrap().to_owned();
        text.push_str(line.strip_suffix('c').unwrap());
        fs::write(&file, text).unwrap();

        let cache = open(&tmp);
        assert!(cached_paths(&cache, vec![info("/r/ab", 5, 100)]).is_empty());
        assert_eq!(cached_paths(&cache, vec![info("/r/abc", 5, 100)]).len(), 1);
    }

    #[test]
    fn unknown_header_discards_cache() {
        let tmp = TempDir::new().unwrap();
        open(&tmp).save(Path::new("/r"), &[hashed("/r/a", 5, Some(100), 9)]);
        let file = cache_file(&tmp);
        let text = fs::read_to_string(&file)
            .unwrap()
            .replace(HEADER, "dedupe-cache v0");
        fs::write(&file, text).unwrap();

        assert!(cached_paths(&open(&tmp), vec![info("/r/a", 5, 100)]).is_empty());
    }

    #[test]
    fn paths_with_tabs_round_trip_and_newlines_are_not_stored() {
        let tmp = TempDir::new().unwrap();
        let tab = hashed("/r/a\tb", 5, Some(100), 1);
        let newline = hashed("/r/a\nb", 5, Some(100), 2);
        open(&tmp).save(Path::new("/r"), &[tab, newline]);

        let cache = open(&tmp);
        assert_eq!(cached_paths(&cache, vec![info("/r/a\tb", 5, 100)]).len(), 1);
        assert!(cached_paths(&cache, vec![info("/r/a\nb", 5, 100)]).is_empty());
        assert_eq!(
            fs::read_to_string(cache_file(&tmp))
                .unwrap()
                .lines()
                .count(),
            2
        );
    }

    #[test]
    fn files_without_mtime_are_not_stored() {
        let tmp = TempDir::new().unwrap();
        let cache = open(&tmp);
        cache.record(&hashed("/r/a", 5, None, 1));
        cache.save(Path::new("/r"), &[hashed("/r/b", 5, None, 1)]);

        assert_eq!(
            fs::read_to_string(cache_file(&tmp)).unwrap(),
            format!("{HEADER}\n")
        );
    }

    #[test]
    fn without_a_location_the_cache_remembers_nothing() {
        let cache = HashCache::open_at(None);
        cache.record(&hashed("/r/a", 5, Some(100), 1));
        assert!(cached_paths(&cache, vec![info("/r/a", 5, 100)]).is_empty());
        cache.save(Path::new("/r"), &[]);
    }

    #[test]
    fn missing_cache_file_is_created() {
        let tmp = TempDir::new().unwrap();
        let cache = open(&tmp);
        assert!(cache_file(&tmp).exists());
        assert!(cached_paths(&cache, vec![info("/r/a", 5, 100)]).is_empty());
    }
}
