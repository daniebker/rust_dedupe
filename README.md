# dedupe

Recursively finds duplicate files in a folder and deletes them, or moves them to
another folder. Files are compared by BLAKE3 hash, and hashing runs in parallel
across all CPU cores.

## Build

Requires Rust 1.79 or newer. Install it with [rustup](https://rustup.rs):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then build:

```bash
cargo build --release      # binary at target/release/dedupe
cargo install --path .     # or install to ~/.cargo/bin as `dedupe`
```

## Usage

```
dedupe [OPTIONS] <PATH>
```

| Option | Description |
|---|---|
| `-n`, `--dry-run` | Print what would happen without changing any files |
| `-m`, `--move <DEST>` | Move duplicates into `DEST` instead of deleting them |
| `-j`, `--threads <N>` | Number of hashing threads (default: 2× CPU cores) |
| `--no-cache` | Ignore cached hashes and rehash every file (the cache is still updated) |
| `-h`, `--help` | Show help |
| `-V`, `--version` | Show version |

Always start with a dry run:

```bash
dedupe ~/Pictures -n               # preview what would be deleted (and cache hashes)
dedupe ~/Pictures                  # delete duplicates, reusing the cached hashes
dedupe ~/Pictures -m ~/dupes -n    # preview moving duplicates to ~/dupes
dedupe ~/Pictures -m ~/dupes       # move duplicates to ~/dupes
dedupe /mnt/hdd -j 2               # fewer threads for a spinning hard drive
```

Example output:

```
keep /home/me/Pictures/a/beach.jpg
  [dry run] would delete /home/me/Pictures/b/beach copy.jpg
  [dry run] would delete /home/me/Pictures/c/beach.jpg

1 duplicate groups; would delete 2 files (4821933 bytes).
```

## How it works

1. **Scan:** walks the folder and groups files by size. A file with a unique
   size can't have a duplicate, so most files are never read.
2. **Hash:** reuses cached hashes for files that haven't changed (see
   [Hash cache](#hash-cache)) and hashes the rest with BLAKE3 in parallel. Large files are
   memory-mapped and split across all cores, so even one big file uses the
   whole CPU.
3. **Group:** files with the same size and hash are duplicates.
4. **Act:** in each group, keeps the file whose path sorts first alphabetically
   and deletes or moves the rest.

Progress is shown on stderr: a running count of files found while scanning,
then a breakdown of where those files went, then a bar showing files hashed out
of the total to hash:

```
Scanned 59202 files: 41870 unique size, 713 empty, 0 cached, 16619 to hash
```
 The progress display
is hidden when stderr isn't a terminal, so `dedupe ~/x -n > report.txt` gives a
clean list of actions.

## Hash cache

Every run, including a dry run, saves each hashed file's path, size,
modification time and hash to `~/.cache/dedupe/hashes.tsv` (or
`$XDG_CACHE_HOME/dedupe/hashes.tsv`). On later runs, a file whose size and
modification time are unchanged reuses its cached hash instead of being read
again. That makes the real run after a dry run fast: only the folder scan is
repeated.

- Hashes are saved as they're computed, so a run that is stopped partway
  (Ctrl-C, crash, reboot) resumes where it left off: the next run only hashes
  the files the stopped run hadn't reached.
- Any file that was edited is rehashed automatically.
- Running on one folder doesn't disturb cached hashes for other folders.
- A program that changes a file's contents and then restores its old
  modification time would fool the cache. Use `--no-cache` if you suspect that.
- Deleting the cache file is always safe; it is rebuilt on the next run.

## Details

- **Moving keeps the folder structure.** `<PATH>/a/b.txt` goes to
  `<DEST>/a/b.txt`. If that name is taken, `_1`, `_2`, … is added. Moves across
  filesystems copy the file and then delete the original.
- **The destination can be inside the scanned folder.** It is skipped during the
  scan.
- **Empty files are ignored.** They would all match each other, and deleting
  them would remove placeholders like `.gitkeep`.
- **Symlinks are not followed** and are never deleted or moved.
- **Errors don't stop the run.** A file that can't be read, deleted or moved is
  reported as a warning, and the run continues.

## Performance

- On SSDs and NVMe drives, the default of 2× cores keeps the CPU busy while
  some threads wait on the disk. Try a higher `-j` on fast drives.
- On spinning hard drives, the disk is the limit. Many threads make the drive
  jump between files, so `-j 1` or `-j 2` is usually faster.

## Caveats

- Files are memory-mapped while being hashed. If another program shortens a file
  during the run, `dedupe` can crash. Don't run it on folders that are being
  written to.
- Deletion is permanent. Use `-n` first, or `-m` to move duplicates somewhere
  you can review them before deleting.

## Development

```bash
cargo test                    # unit + integration tests (temp folders; never touch your real cache)
cargo test --test cli         # only the integration tests, which run the real binary
cargo clippy --all-targets    # lints
```

## License

MIT. See [LICENSE](LICENSE).
