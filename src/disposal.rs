//! What happens to the extra copies of a duplicate.
//!
//! A [`Disposal`] strategy turns each extra copy into a [`Plan`]: a
//! description plus a deferred operation. A real run prints and executes the
//! plans; a dry run only prints them, so no strategy needs to know about dry
//! runs and a new strategy only has to implement `Disposal`.

use crate::duplicates::DuplicateGroup;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct Verbs {
    pub present: &'static str,
    pub past: &'static str,
}

pub trait Disposal {
    fn verbs(&self) -> Verbs;
    /// Works out what will happen to `file` without touching the disk.
    fn plan(&self, file: &Path) -> Plan;
}

pub struct Plan {
    description: String,
    operation: Box<dyn FnOnce() -> io::Result<()>>,
}

impl Plan {
    fn new(description: String, operation: impl FnOnce() -> io::Result<()> + 'static) -> Self {
        Plan { description, operation: Box::new(operation) }
    }
}

pub struct Delete;

impl Disposal for Delete {
    fn verbs(&self) -> Verbs {
        Verbs { present: "delete", past: "deleted" }
    }

    fn plan(&self, file: &Path) -> Plan {
        let file = file.to_owned();
        Plan::new(format!("delete {}", file.display()), move || fs::remove_file(file))
    }
}

/// Moves files under `dest`, mirroring their path relative to `root`.
pub struct Move {
    pub root: PathBuf,
    pub dest: PathBuf,
}

impl Disposal for Move {
    fn verbs(&self) -> Verbs {
        Verbs { present: "move", past: "moved" }
    }

    fn plan(&self, file: &Path) -> Plan {
        let from = file.to_owned();
        let to = self.target(file);
        let description = format!("move {} -> {}", from.display(), to.display());
        Plan::new(description, move || move_file(&from, &to))
    }
}

impl Move {
    /// Adds a numeric suffix if something is already at the mirrored path.
    fn target(&self, file: &Path) -> PathBuf {
        let rel = file.strip_prefix(&self.root).unwrap_or(file);
        let target = self.dest.join(rel);
        if !target.exists() {
            return target;
        }
        let stem = target.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let ext = target
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        (1..)
            .map(|_| target.clone())
            .find(|p| !p.exists())
            .unwrap()
    }
}

fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    // rename fails across filesystems; fall back to copy + delete.
    if fs::rename(from, to).is_err() {
        fs::copy(from, to)?;
        fs::remove_file(from)?;
    }
    Ok(())
}

pub struct Summary {
    groups: usize,
    verb: String,
    files: usize,
    bytes: u64,
    failures: usize,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let Summary { groups, verb, files, bytes, failures } = self;
        write!(f, "{groups} duplicate groups; {verb} {files} files ({bytes} bytes).")?;
        if *failures > 0 {
            write!(f, "\n{failures} files failed.")?;
        }
        Ok(())
    }
}

/// Keeps the first file of each group and disposes of the rest, printing each
/// step. With `dry_run`, only prints what would happen.
pub fn dispose(groups: &[DuplicateGroup], disposal: &dyn Disposal, dry_run: bool) -> Summary {
    let verbs = disposal.verbs();
    let (prefix, verb) = if dry_run {
        ("[dry run] would ", format!("would {}", verbs.present))
    } else {
        ("", verbs.past.to_owned())
    };
    let mut summary = Summary { groups: groups.len(), verb, files: 0, bytes: 0, failures: 0 };

    for group in groups {
        println!("keep {}", group.keep.display());
        for file in &group.extras {
            let plan = disposal.plan(file);
            println!("  {prefix}{}", plan.description);
            let result = if dry_run { Ok(()) } else { (plan.operation)() };
            match result {
                Ok(()) => {
                    summary.files += 1;
                    summary.bytes += group.size;
                }
                Err(e) => {
                    summary.failures += 1;
                    eprintln!("  error: {}: {e}", file.display());
                }
            }
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn mover(tmp: &TempDir) -> Move {
        Move { root: tmp.path().join("root"), dest: tmp.path().join("dest") }
    }

    #[test]
    fn move_target_mirrors_path_under_dest() {
        let tmp = TempDir::new().unwrap();
        let m = mover(&tmp);
        assert_eq!(m.target(&m.root.join("a/b/photo.jpg")), m.dest.join("a/b/photo.jpg"));
    }

    #[test]
    fn move_target_adds_suffix_when_taken() {
        let tmp = TempDir::new().unwrap();
        let m = mover(&tmp);
        write(&m.dest.join("a/photo.jpg"), "x");
        assert_eq!(m.target(&m.root.join("a/photo.jpg")), m.dest.join("a/photo_1.jpg"));
        write(&m.dest.join("a/photo_1.jpg"), "x");
        assert_eq!(m.target(&m.root.join("a/photo.jpg")), m.dest.join("a/photo_2.jpg"));
    }

    #[test]
    fn move_target_suffix_without_extension() {
        let tmp = TempDir::new().unwrap();
        let m = mover(&tmp);
        write(&m.dest.join("README"), "x");
        assert_eq!(m.target(&m.root.join("README")), m.dest.join("README_1"));
    }

    #[test]
    fn move_plan_describes_and_moves_file() {
        let tmp = TempDir::new().unwrap();
        let m = mover(&tmp);
        let file = m.root.join("a/f.txt");
        write(&file, "data");

        let plan = m.plan(&file);
        let expected = format!("move {} -> {}", file.display(), m.dest.join("a/f.txt").display());
        assert_eq!(plan.description, expected);
        assert!(file.exists(), "planning must not touch the disk");

        (plan.operation)().unwrap();
        assert!(!file.exists());
        assert_eq!(fs::read_to_string(m.dest.join("a/f.txt")).unwrap(), "data");
    }

    #[test]
    fn delete_plan_describes_and_deletes_file() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("f");
        write(&file, "data");

        let plan = Delete.plan(&file);
        assert_eq!(plan.description, format!("delete {}", file.display()));
        assert!(file.exists(), "planning must not touch the disk");

        (plan.operation)().unwrap();
        assert!(!file.exists());
    }

    /// Three 4-byte copies: `keep` plus extras `x1` and `x2`.
    fn group_of_three(tmp: &TempDir) -> DuplicateGroup {
        let [keep, x1, x2] = ["keep", "x1", "x2"].map(|n| tmp.path().join(n));
        for f in [&keep, &x1, &x2] {
            write(f, "same");
        }
        DuplicateGroup { size: 4, keep, extras: vec![x1, x2] }
    }

    #[test]
    fn dry_run_changes_nothing_but_reports_what_would_happen() {
        let tmp = TempDir::new().unwrap();
        let group = group_of_three(&tmp);

        let summary = dispose(std::slice::from_ref(&group), &Delete, true);

        assert!(group.keep.exists() && group.extras.iter().all(|f| f.exists()));
        assert_eq!(summary.to_string(), "1 duplicate groups; would delete 2 files (8 bytes).");
    }

    #[test]
    fn real_run_deletes_extras_and_keeps_first() {
        let tmp = TempDir::new().unwrap();
        let group = group_of_three(&tmp);

        let summary = dispose(std::slice::from_ref(&group), &Delete, false);

        assert!(group.keep.exists());
        assert!(group.extras.iter().all(|f| !f.exists()));
        assert_eq!(summary.to_string(), "1 duplicate groups; deleted 2 files (8 bytes).");
    }

    #[test]
    fn failures_are_counted_and_do_not_stop_the_run() {
        let tmp = TempDir::new().unwrap();
        let mut group = group_of_three(&tmp);
        // An extra in a read-only folder can't be deleted.
        let locked_dir = tmp.path().join("locked");
        let locked = locked_dir.join("x0");
        write(&locked, "same");
        fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o555)).unwrap();
        group.extras.insert(0, locked.clone());

        let summary = dispose(std::slice::from_ref(&group), &Delete, false);
        fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(locked.exists());
        assert!(group.extras[1..].iter().all(|f| !f.exists()));
        assert_eq!(
            summary.to_string(),
            "1 duplicate groups; deleted 2 files (8 bytes).\n1 files failed."
        );
    }
}
