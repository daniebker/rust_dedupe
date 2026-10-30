//! End-to-end tests: run the real binary against temporary folders.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// A temporary folder to deduplicate (`root`), plus a private cache folder so
/// tests never touch the real cache or each other's.
struct Fixture {
    tmp: TempDir,
}

struct Run {
    success: bool,
    stdout: String,
    stderr: String,
}

impl Fixture {
    fn new() -> Self {
        let f = Fixture {
            tmp: TempDir::new().unwrap(),
        };
        fs::create_dir(f.root()).unwrap();
        f
    }

    /// Canonical, because that's how the binary prints paths.
    fn root(&self) -> PathBuf {
        fs::canonicalize(self.tmp.path()).unwrap().join("root")
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root().join(rel)
    }

    fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let path = self.path(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn run(&self, args: &[&str]) -> Run {
        self.run_on(&self.root(), args)
    }

    fn run_on(&self, target: &Path, args: &[&str]) -> Run {
        let out = Command::new(env!("CARGO_BIN_EXE_dedupe"))
            .arg(target)
            .args(args)
            .env("XDG_CACHE_HOME", self.tmp.path().join("cache"))
            .output()
            .unwrap();
        Run {
            success: out.status.success(),
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
        }
    }

    fn cache_contents(&self) -> String {
        fs::read_to_string(self.tmp.path().join("cache/dedupe/hashes.tsv")).unwrap()
    }

    /// All files under root, relative and sorted.
    fn files(&self) -> Vec<String> {
        files_under(&self.root())
    }
}

fn files_under(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() && !path.is_symlink() {
                stack.push(path);
            } else {
                files.push(
                    path.strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    files.sort();
    files
}

/// Three copies of one file plus an unrelated file of the same size.
fn three_copies(f: &Fixture) {
    f.write("a/photo.jpg", "same");
    f.write("b/photo.jpg", "same");
    f.write("c/copy.jpg", "same");
    f.write("c/other.jpg", "diff");
}

#[test]
fn dry_run_reports_but_changes_nothing() {
    let f = Fixture::new();
    three_copies(&f);

    let run = f.run(&["-n"]);

    assert!(run.success);
    let expected = format!(
        "keep {a}\n  [dry run] would delete {b}\n  [dry run] would delete {c}\n\n\
         1 duplicate groups; would delete 2 files (8 bytes).\n",
        a = f.path("a/photo.jpg").display(),
        b = f.path("b/photo.jpg").display(),
        c = f.path("c/copy.jpg").display(),
    );
    assert_eq!(run.stdout, expected);
    assert_eq!(
        f.files(),
        ["a/photo.jpg", "b/photo.jpg", "c/copy.jpg", "c/other.jpg"]
    );
}

#[test]
fn deletes_extras_and_keeps_first_path() {
    let f = Fixture::new();
    three_copies(&f);

    let run = f.run(&[]);

    assert!(run.success);
    assert!(run
        .stdout
        .ends_with("1 duplicate groups; deleted 2 files (8 bytes).\n"));
    assert_eq!(f.files(), ["a/photo.jpg", "c/other.jpg"]);
    assert_eq!(fs::read_to_string(f.path("a/photo.jpg")).unwrap(), "same");
}

#[test]
fn move_mirrors_folder_structure_into_dest() {
    let f = Fixture::new();
    three_copies(&f);
    let dest = f.tmp.path().join("dupes");

    let run = f.run(&["-m", dest.to_str().unwrap()]);

    assert!(run.success, "{}", run.stderr);
    assert!(run
        .stdout
        .ends_with("1 duplicate groups; moved 2 files (8 bytes).\n"));
    assert_eq!(f.files(), ["a/photo.jpg", "c/other.jpg"]);
    assert_eq!(files_under(&dest), ["b/photo.jpg", "c/copy.jpg"]);
    assert_eq!(
        fs::read_to_string(dest.join("b/photo.jpg")).unwrap(),
        "same"
    );
}

#[test]
fn move_renames_on_clash_in_dest() {
    let f = Fixture::new();
    f.write("a/x.txt", "same");
    f.write("b/x.txt", "same");
    let dest = f.tmp.path().join("dupes");
    fs::create_dir_all(dest.join("b")).unwrap();
    fs::write(dest.join("b/x.txt"), "already here").unwrap();

    assert!(f.run(&["-m", dest.to_str().unwrap()]).success);

    assert_eq!(files_under(&dest), ["b/x.txt", "b/x_1.txt"]);
    assert_eq!(
        fs::read_to_string(dest.join("b/x.txt")).unwrap(),
        "already here"
    );
    assert_eq!(fs::read_to_string(dest.join("b/x_1.txt")).unwrap(), "same");
}

#[test]
fn dest_inside_root_is_not_rescanned() {
    let f = Fixture::new();
    three_copies(&f);
    let dest = f.path("dupes");
    let dest = dest.to_str().unwrap();

    assert!(f.run(&["-m", dest]).success);
    // The moved copies are still duplicates of a/photo.jpg, but live in dest.
    let second = f.run(&["-m", dest]);

    assert_eq!(second.stdout, "No duplicates found.\n");
    assert_eq!(
        f.files(),
        [
            "a/photo.jpg",
            "c/other.jpg",
            "dupes/b/photo.jpg",
            "dupes/c/copy.jpg"
        ]
    );
}

#[test]
fn real_run_after_dry_run_uses_cached_hashes() {
    let f = Fixture::new();
    three_copies(&f);

    let dry = f.run(&["-n"]);
    let real = f.run(&[]);

    assert!(dry.stderr.contains("0 cached, 4 to hash"), "{}", dry.stderr);
    assert!(
        real.stderr.contains("4 cached, 0 to hash"),
        "{}",
        real.stderr
    );
    assert_eq!(f.files(), ["a/photo.jpg", "c/other.jpg"]);
}

#[test]
fn file_edited_after_dry_run_is_rehashed_not_deleted() {
    let f = Fixture::new();
    three_copies(&f);
    assert!(f.run(&["-n"]).stdout.contains("would delete"));

    // Same size, new contents: the cached hash for b/photo.jpg is now wrong.
    let edited = f.path("b/photo.jpg");
    std::thread::sleep(std::time::Duration::from_millis(10)); // ensure a new mtime
    fs::write(&edited, "SAME").unwrap();
    let run = f.run(&[]);

    assert!(run.stderr.contains("3 cached, 1 to hash"), "{}", run.stderr);
    assert_eq!(fs::read_to_string(&edited).unwrap(), "SAME");
    assert_eq!(f.files(), ["a/photo.jpg", "b/photo.jpg", "c/other.jpg"]);
}

#[test]
fn no_cache_rehashes_everything() {
    let f = Fixture::new();
    three_copies(&f);
    f.run(&["-n"]);

    let run = f.run(&["-n", "--no-cache"]);

    assert!(run.stderr.contains("0 cached, 4 to hash"), "{}", run.stderr);
}

#[test]
fn scan_summary_accounts_for_every_file() {
    let f = Fixture::new();
    three_copies(&f);
    f.write("unique.txt", "a size nothing else has");
    f.write("empty1", "");
    f.write("sub/empty2", "");

    let run = f.run(&["-n"]);

    assert!(
        run.stderr
            .contains("Scanned 7 files: 1 unique size, 2 empty, 0 cached, 4 to hash"),
        "{}",
        run.stderr
    );
}

#[test]
fn empty_files_and_symlinks_are_left_alone() {
    let f = Fixture::new();
    f.write("a/.gitkeep", "");
    f.write("b/.gitkeep", "");
    f.write("real", "data");
    std::os::unix::fs::symlink(f.path("real"), f.path("link1")).unwrap();
    std::os::unix::fs::symlink(f.path("real"), f.path("link2")).unwrap();

    let run = f.run(&[]);

    assert_eq!(run.stdout, "No duplicates found.\n");
    assert_eq!(
        f.files(),
        ["a/.gitkeep", "b/.gitkeep", "link1", "link2", "real"]
    );
}

#[test]
fn unreadable_file_warns_and_run_continues() {
    let f = Fixture::new();
    three_copies(&f);
    let locked = f.write("locked.jpg", "same");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&locked).is_ok() {
        return; // Running as root: permissions aren't enforced.
    }

    let run = f.run(&[]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();

    assert!(run.success);
    // stderr is a pipe here, not a terminal: warnings must still appear.
    let warning = format!(
        "warning: could not hash {}: Permission denied",
        locked.display()
    );
    assert!(run.stderr.contains(&warning), "{}", run.stderr);
    assert_eq!(f.files(), ["a/photo.jpg", "c/other.jpg", "locked.jpg"]);
}

#[test]
fn failed_delete_is_reported_and_others_proceed() {
    let f = Fixture::new();
    three_copies(&f);
    let locked_dir = f.path("b");
    fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let run = f.run(&[]);
    fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o755)).unwrap();
    if !f.path("b/photo.jpg").exists() {
        return; // Running as root: permissions aren't enforced.
    }

    assert!(run.success);
    assert!(run.stderr.contains("Permission denied"), "{}", run.stderr);
    assert!(
        run.stdout
            .ends_with("deleted 1 files (4 bytes).\n1 files failed.\n"),
        "{}",
        run.stdout
    );
    assert_eq!(f.files(), ["a/photo.jpg", "b/photo.jpg", "c/other.jpg"]);
}

#[test]
fn thread_count_does_not_change_the_result() {
    let f = Fixture::new();
    three_copies(&f);

    let one = f.run(&["-n", "--no-cache", "-j", "1"]);
    let many = f.run(&["-n", "--no-cache", "-j", "16"]);

    assert_eq!(one.stdout, many.stdout);
}

#[test]
fn missing_folder_is_an_error() {
    let f = Fixture::new();

    let run = f.run_on(&f.tmp.path().join("nope"), &[]);

    assert!(!run.success);
    assert!(run.stderr.starts_with("error: "), "{}", run.stderr);
}

#[test]
fn file_instead_of_folder_is_an_error() {
    let f = Fixture::new();
    let file = f.write("file.txt", "x");

    let run = f.run_on(&file, &[]);

    assert!(!run.success);
    assert!(run.stderr.contains("is not a directory"), "{}", run.stderr);
}

#[test]
fn killed_run_resumes_from_hashes_saved_so_far() {
    let f = Fixture::new();
    // Enough data that a single-threaded run is still hashing when we kill it.
    let data = "x".repeat(1_000_000);
    for i in 0..300 {
        f.write(&format!("f{i:03}"), &data);
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_dedupe"))
        .args([f.root().to_str().unwrap(), "-n", "-j", "1"])
        .env("XDG_CACHE_HOME", f.tmp.path().join("cache"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let cache = f.tmp.path().join("cache/dedupe/hashes.tsv");
    // Wait until at least one hash has been saved (header + 1 line).
    while fs::read_to_string(&cache).map_or(0, |c| c.lines().count()) < 2 {
        assert!(
            child.try_wait().unwrap().is_none(),
            "run finished before it could be killed"
        );
    }
    assert!(
        child.try_wait().unwrap().is_none(),
        "run finished before it could be killed"
    );
    child.kill().unwrap(); // SIGKILL: no chance to clean up or run the final save.
    child.wait().unwrap();

    let resumed = f.run(&["-n"]);

    let cached: usize = resumed
        .stderr
        .split(", ")
        .find_map(|part| part.strip_suffix(" cached")?.parse().ok())
        .unwrap();
    assert!((1..300).contains(&cached), "{}", resumed.stderr);
    assert!(resumed
        .stderr
        .contains(&format!("{cached} cached, {} to hash", 300 - cached)));
}

#[test]
fn cache_forgets_deleted_duplicates() {
    let f = Fixture::new();
    three_copies(&f);
    assert!(f.run(&[]).success);

    f.run(&["-n"]);

    let cache = f.cache_contents();
    assert!(
        !cache.contains("b/photo.jpg") && !cache.contains("c/copy.jpg"),
        "{cache}"
    );
}
