//! Reading the files dsh is about to read, from several threads, while it
//! starts.
//!
//! The first time a file is read after it lands on disk, Windows Defender scans
//! it before anyone sees a byte — tens of milliseconds for a file that is
//! otherwise a few kilobytes. dsh imports around two thousand of them one after
//! another, so the scans queue up behind each other: measured on a tree the
//! scanner had never seen, `dsh web` took 14s to start against 1.6s once the
//! same files had been read once. Reading them here, several at a time, lets
//! the scanner work on several at once and brings that first launch down to
//! about four seconds.
//!
//! Nothing waits on this. Every read is thrown away — the point is the state it
//! leaves behind, in the page cache and in the scanner's verdict cache — so a
//! file that has moved or vanished is simply skipped, and a launch where
//! everything is already warm costs a few hundred milliseconds of background
//! I/O and changes nothing.
//!
//! The list is recorded at build time by `scripts/bundle-runtime.mjs`; a build
//! without one warms nothing.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tauri::AppHandle;

/// The widest and narrowest this spreads. Two is enough to be worth starting;
/// past eight there is nothing left to win — see [`threads`].
const THREADS: std::ops::RangeInclusive<usize> = 2..=8;

/// Start warming the dsh that is about to run. Returns immediately.
pub fn start(app: &AppHandle) {
    let app = app.clone();

    std::thread::spawn(move || {
        let Some(files) = plan(&app) else { return };
        read_all(files);
    });
}

/// The absolute path of every file in the warm-up list, against the install
/// that is about to be started.
///
/// The list is recorded against a local `npm install`, whose paths start at
/// `node_modules/`. A global install lays the same tree out under its prefix, so
/// the root here is the directory holding dsh's `node_modules` either way.
fn plan(app: &AppHandle) -> Option<Vec<PathBuf>> {
    let list = crate::dsh::resources(app)?.join("dsh-boot-set.txt");
    let root = crate::dsh::current(app)?.root;

    let files = std::fs::read_to_string(list).ok()?;
    Some(
        files
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| root.join(line))
            .collect(),
    )
}

/// How wide to go on this machine.
///
/// Each thread spends its time blocked on an open the scanner is busy with, so
/// what this really sets is how many scans can be in flight. Measured against a
/// tree the scanner had never seen: one thread 12.9s, two 5.3s, four 3.1s,
/// eight 1.8s, twelve 1.5s. The gains flatten around one thread per core, and
/// past that the only thing a bigger number buys is more of a small machine
/// spent on a warm-up nobody is waiting for — while dsh, which someone *is*
/// waiting for, wants the same cores.
fn threads() -> usize {
    let cores = std::thread::available_parallelism().map_or(*THREADS.start(), |cores| cores.get());
    cores.clamp(*THREADS.start(), *THREADS.end())
}

/// Read every file, spreading them over [`threads`] threads that each take the
/// next one that has not been claimed.
fn read_all(files: Vec<PathBuf>) {
    let files = Arc::new(files);
    let next = Arc::new(AtomicUsize::new(0));

    for _ in 0..threads().min(files.len()) {
        let files = files.clone();
        let next = next.clone();

        std::thread::spawn(move || loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(file) = files.get(index) else { return };
            read(file);
        });
    }
}

/// Read one file and drop it on the floor.
fn read(file: &Path) {
    if let Ok(mut file) = File::open(file) {
        let _ = std::io::copy(&mut file, &mut std::io::sink());
    }
}
