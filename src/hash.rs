//! Parallel BLAKE3 hashing of candidate files.

use crate::progress;
use crate::scan::FileInfo;
use rayon::prelude::*;
use std::io;
use std::path::Path;

pub type Hash = [u8; 32];

pub struct HashedFile {
    pub file: FileInfo,
    pub hash: Hash,
}

/// Hashes `files` in parallel with a progress bar, calling `on_hashed` as
/// each one finishes. Files that can't be read are reported and left out.
pub fn hash_all(files: Vec<FileInfo>, on_hashed: impl Fn(&HashedFile) + Sync) -> Vec<HashedFile> {
    let bar = progress::bar(
        files.len(),
        "{spinner} Hashing [{bar:40}] {pos}/{len} files ({percent}%, ETA {eta})",
    );
    let hashed = files
        .into_par_iter()
        .filter_map(|file| {
            let result = match hash_file(&file.path) {
                Ok(hash) => {
                    let hashed = HashedFile { file, hash };
                    on_hashed(&hashed);
                    Some(hashed)
                }
                Err(e) => {
                    progress::warn(&bar, format!("could not hash {}: {e}", file.path.display()));
                    None
                }
            };
            bar.inc(1);
            result
        })
        .collect();
    bar.finish();
    hashed
}

/// Memory-maps the file and splits large ones across the rayon pool, so a
/// single big file uses every core. Small files fall back to plain reads.
fn hash_file(path: &Path) -> io::Result<Hash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_mmap_rayon(path)?;
    Ok(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn file_info(path: std::path::PathBuf, size: u64) -> FileInfo {
        FileInfo {
            path,
            size,
            mtime: None,
        }
    }

    #[test]
    fn hashes_match_blake3_and_callback_sees_each_file() {
        let tmp = TempDir::new().unwrap();
        // Includes a file big enough to take the memory-mapped, multi-threaded path.
        let big = vec![7u8; 2_000_000];
        let contents: [&[u8]; 3] = [b"hello", b"world", &big];
        let files: Vec<FileInfo> = contents
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let path = tmp.path().join(i.to_string());
                fs::write(&path, c).unwrap();
                file_info(path, c.len() as u64)
            })
            .collect();

        let seen = Mutex::new(Vec::new());
        let mut hashed = hash_all(files, |h| seen.lock().unwrap().push(h.file.path.clone()));

        hashed.sort_by(|a, b| a.file.path.cmp(&b.file.path));
        for (h, c) in hashed.iter().zip(contents) {
            assert_eq!(h.hash, *blake3::hash(c).as_bytes());
        }
        assert_eq!(hashed.len(), 3);
        assert_eq!(seen.into_inner().unwrap().len(), 3);
    }

    #[test]
    fn unreadable_files_are_left_out() {
        let tmp = TempDir::new().unwrap();
        let ok = tmp.path().join("ok");
        let locked = tmp.path().join("locked");
        fs::write(&ok, "data").unwrap();
        fs::write(&locked, "data").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&locked).is_ok() {
            return; // Running as root: permissions aren't enforced.
        }

        let hashed = hash_all(vec![file_info(ok.clone(), 4), file_info(locked, 4)], |_| {});

        assert_eq!(hashed.len(), 1);
        assert_eq!(hashed[0].file.path, ok);
    }
}
