//! Groups hashed files into sets of identical files.

use crate::hash::{Hash, HashedFile};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct DuplicateGroup {
    pub size: u64,
    /// The copy to keep: the path that sorts first.
    pub keep: PathBuf,
    /// The other copies, in path order.
    pub extras: Vec<PathBuf>,
}

/// Files with the same size and hash are duplicates. Groups come back in
/// order of the path they keep, so output is stable between runs.
pub fn find(files: Vec<HashedFile>) -> Vec<DuplicateGroup> {
    let mut by_content: HashMap<(u64, Hash), Vec<PathBuf>> = HashMap::new();
    for h in files {
        by_content
            .entry((h.file.size, h.hash))
            .or_default()
            .push(h.file.path);
    }
    let mut groups: Vec<DuplicateGroup> = by_content
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|((size, _), mut paths)| {
            paths.sort();
            let keep = paths.remove(0);
            DuplicateGroup {
                size,
                keep,
                extras: paths,
            }
        })
        .collect();
    groups.sort_by(|a, b| a.keep.cmp(&b.keep));
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::FileInfo;

    fn hashed(path: &str, size: u64, hash_byte: u8) -> HashedFile {
        let file = FileInfo {
            path: path.into(),
            size,
            mtime: None,
        };
        HashedFile {
            file,
            hash: [hash_byte; 32],
        }
    }

    #[test]
    fn groups_identical_files_and_keeps_first_path() {
        let groups = find(vec![
            hashed("/z/copy", 10, 1),
            hashed("/a/orig", 10, 1),
            hashed("/m/copy", 10, 1),
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].size, 10);
        assert_eq!(groups[0].keep, PathBuf::from("/a/orig"));
        assert_eq!(
            groups[0].extras,
            [PathBuf::from("/m/copy"), PathBuf::from("/z/copy")]
        );
    }

    #[test]
    fn files_without_a_match_are_not_grouped() {
        let groups = find(vec![
            hashed("/a", 10, 1),
            hashed("/b", 10, 2),
            hashed("/c", 10, 3),
        ]);
        assert!(groups.is_empty());
    }

    #[test]
    fn same_hash_with_different_size_is_not_a_duplicate() {
        assert!(find(vec![hashed("/a", 10, 1), hashed("/b", 20, 1)]).is_empty());
    }

    #[test]
    fn groups_are_ordered_by_kept_path() {
        let groups = find(vec![
            hashed("/c/1", 1, 1),
            hashed("/c/2", 1, 1),
            hashed("/a/1", 2, 2),
            hashed("/a/2", 2, 2),
            hashed("/b/1", 3, 3),
            hashed("/b/2", 3, 3),
        ]);
        let kept: Vec<_> = groups.iter().map(|g| g.keep.to_str().unwrap()).collect();
        assert_eq!(kept, ["/a/1", "/b/1", "/c/1"]);
    }

    #[test]
    fn no_input_gives_no_groups() {
        assert!(find(Vec::new()).is_empty());
    }
}
