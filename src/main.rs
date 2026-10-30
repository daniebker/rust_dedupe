mod cache;
mod disposal;
mod duplicates;
mod hash;
mod progress;
mod scan;

use cache::HashCache;
use clap::Parser;
use disposal::{Delete, Disposal, Move};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Recursively find duplicate files (by BLAKE3 hash) and delete or move the extras.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Folder to scan recursively
    path: PathBuf,

    /// Only print what would happen; don't touch any files. Hashes are still
    /// cached, so a following real run is fast
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Ignore cached hashes and rehash every file (the cache is still updated)
    #[arg(long)]
    no_cache: bool,

    /// Move duplicates into this folder instead of deleting them
    #[arg(short = 'm', long = "move", value_name = "DEST")]
    move_to: Option<PathBuf>,

    /// Number of hashing threads [default: 2x CPU cores]. Extra threads keep
    /// the CPU busy while others wait on disk; use 1-2 on spinning hard drives.
    #[arg(short = 'j', long, value_name = "N")]
    threads: Option<usize>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let threads = args.threads.unwrap_or_else(|| {
        2 * std::thread::available_parallelism().map_or(4, |n| n.get())
    });
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads.max(1))
        .build_global()
        .expect("failed to build thread pool");
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> io::Result<()> {
    let root = resolve_dir(&args.path)?;
    // Absolute, so the scan can skip it if it lives inside root.
    let dest = args.move_to.as_deref().map(std::path::absolute).transpose()?;

    let scan = scan::scan(&root, dest.as_deref());

    let cache = HashCache::open();
    let (mut hashed, to_hash) = if args.no_cache {
        (Vec::new(), scan.candidates)
    } else {
        cache.split_cached(scan.candidates)
    };
    eprintln!("{}, {} cached, {} to hash", scan.stats, hashed.len(), to_hash.len());
    hashed.extend(hash::hash_all(to_hash, |h| cache.record(h)));
    cache.save(&root, &hashed);

    let groups = duplicates::find(hashed);
    if groups.is_empty() {
        println!("No duplicates found.");
        return Ok(());
    }

    let strategy: Box<dyn Disposal> = match dest {
        Some(dest) => Box::new(Move { root, dest }),
        None => Box::new(Delete),
    };
    let summary = disposal::dispose(&groups, strategy.as_ref(), args.dry_run);
    println!("\n{summary}");
    Ok(())
}

fn resolve_dir(path: &Path) -> io::Result<PathBuf> {
    let dir = fs::canonicalize(path)?;
    if !dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", dir.display()),
        ));
    }
    Ok(dir)
}
