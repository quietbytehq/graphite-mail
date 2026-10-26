//! Crash-safe move of the data folder (database + saved mail) to a new place.
//!
//! Seven phases. Until phase 6 the old folder stays the only real copy; the single point of no return is
//! one atomic rewrite of bootstrap.json. If the app or computer dies at any moment, `recover()` (run at
//! start-up) either undoes the half-made copy or finishes tidying up, so mail is never lost or split.
//!
//!  1. Check the new folder (empty, writable, outside the old one, enough space)
//!  2. Write the intention into bootstrap.json (state COPYING)
//!  3. Copy saved mail (blobs) while the app keeps working; every file is read back and verified
//!  4. Briefly pause the database, copy any newer mail, snapshot the database (VACUUM INTO)
//!  5. Check the copy (database integrity, row counts, every file present and the right size)
//!  6. Switch: move the copy into place, open it, then atomically point bootstrap.json at it (state SWITCHED)
//!  7. Clean up the old folder (only our own folders), state back to IDLE

use crate::storage::{self, Bootstrap};
use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const OWN_DIRS: [&str; 3] = ["db", "blobs", "temp_staging"];
const STAGING: &str = ".graphite-moving";
const TABLES: [&str; 7] = [
    "accounts",
    "folders",
    "messages",
    "attachments",
    "calendars",
    "calendar_events",
    "offline_mutation_queue",
];

#[derive(Serialize, Clone)]
pub struct Progress {
    pub phase: u8,
    pub label: String,
    pub done: u64,
    pub total: u64,
}

#[derive(Serialize)]
pub struct Info {
    pub path: String,
    pub size_bytes: u64,
}

pub fn info() -> Result<Info> {
    let b = storage::read_bootstrap().ok_or(anyhow!("Not set up yet"))?;
    Ok(Info {
        size_bytes: dir_size(Path::new(&b.active_data_root)),
        path: b.active_data_root,
    })
}

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(p) else { return 0 };
    rd.filter_map(|e| e.ok())
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(all(test, windows))]
pub(crate) fn normalize_for_test(p: &Path) -> Result<PathBuf> {
    normalize(p)
}

/// Canonical form of a path that may not exist yet (canonicalise the part that does). Uses `dunce`, so on Windows
/// the result is `C:\Users\…` rather than `\\?\C:\Users\…`: this value is stored in bootstrap.json and shown in Settings.
fn normalize(p: &Path) -> Result<PathBuf> {
    let mut rest = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if let Ok(c) = dunce::canonicalize(&cur) {
            return Ok(rest.iter().rev().fold(c, |acc, part| acc.join(part)));
        }
        match (
            cur.file_name().map(|f| f.to_owned()),
            cur.parent().map(|x| x.to_path_buf()),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                cur = parent;
            }
            _ => bail!("That folder location isn't valid."),
        }
    }
}

/// Free bytes on the volume holding `p` (or its nearest existing parent). None when it can't be found out.
pub(crate) fn free_space(p: &Path) -> Option<u64> {
    let mut probe = p;
    while !probe.exists() {
        probe = probe.parent()?;
    }
    free_space_at(probe)
}

#[cfg(unix)]
fn free_space_at(probe: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(probe.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0)
        .then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

#[cfg(windows)]
fn free_space_at(probe: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = probe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free_for_caller: u64 = 0;
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free_for_caller, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free_for_caller)
}

fn sync_dir(p: &Path) {
    let _ = storage::sync_dir(p); // best effort here: the copy is verified afterwards anyway
}

/// Copies one file, reads it back from the new place and compares. Returns its size.
fn copy_verified(src: &Path, dst: &Path) -> Result<u64> {
    if let Some(dir) = dst.parent() {
        storage::private_dir(dir)?;
    }
    let tmp = dst.with_extension("moving-tmp");
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    {
        let mut r =
            fs::File::open(src).with_context(|| format!("Cannot read {}", src.display()))?;
        let mut w =
            storage::private_file(&tmp).with_context(|| format!("Cannot write {}", tmp.display()))?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            w.write_all(&buf[..n])?;
            size += n as u64;
        }
        w.sync_all()?;
    }
    let mut back = Sha256::new();
    let mut r = fs::File::open(&tmp)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        back.update(&buf[..n]);
    }
    if hasher.finalize() != back.finalize() {
        let _ = fs::remove_file(&tmp);
        bail!(
            "A file was not copied correctly ({}). The new folder may have a problem; nothing was changed.",
            src.display()
        );
    }
    fs::rename(&tmp, dst)?;
    Ok(size)
}

/// Every saved-mail file under blobs/, as paths relative to blobs/.
fn list_blobs(root: &Path) -> Vec<PathBuf> {
    let base = root.join("blobs");
    let mut out = Vec::new();
    if let Ok(top) = fs::read_dir(&base) {
        for d in top.filter_map(|e| e.ok()) {
            if let Ok(files) = fs::read_dir(d.path()) {
                for f in files.filter_map(|e| e.ok()) {
                    let rel = PathBuf::from(d.file_name()).join(f.file_name());
                    if !f.file_name().to_string_lossy().ends_with(".tmp") {
                        out.push(rel);
                    }
                }
            }
        }
    }
    out
}

fn remove_own(root: &Path) {
    for d in OWN_DIRS.iter().chain([&STAGING]) {
        let _ = fs::remove_dir_all(root.join(d));
    }
    let _ = fs::remove_dir(root); // only succeeds when nothing else is in it
}

fn set_state(state: &str, target: Option<&str>, previous: Option<&str>) -> Result<()> {
    let mut b = storage::read_bootstrap().ok_or(anyhow!("Not set up yet"))?;
    b.migration_state = state.into();
    b.pending_migration_target = target.map(String::from);
    b.previous_data_root = previous.map(String::from);
    storage::write_bootstrap(&b)
}

/// Marks where a test should stop (simulating a failure or a crash).
#[cfg(test)]
pub static TEST_STOP: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
/// 0 = off; low nibble = phase to stop at; 0x10 = simulate a crash (no clean-up) instead of an error.
#[cfg(test)]
const CRASH: u8 = 0x10;

#[derive(Debug)]
struct Crash;
impl std::fmt::Display for Crash {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "simulated crash")
    }
}
impl std::error::Error for Crash {}

fn checkpoint(_phase: u8) -> Result<()> {
    #[cfg(test)]
    {
        let v = TEST_STOP.load(std::sync::atomic::Ordering::SeqCst);
        if v & 0x0f == _phase && v != 0 {
            return if v & CRASH != 0 {
                Err(Crash.into())
            } else {
                bail!("injected failure in phase {_phase}")
            };
        }
    }
    Ok(())
}

/// Moves everything to `target`. `progress` is called as work advances.
pub fn run(target: &str, progress: impl Fn(Progress)) -> Result<String> {
    // Nothing may be saved mid-move: wait for any running sync and hold new ones off until done.
    let _no_sync = crate::sync::SYNC_LOCK.blocking_lock();
    let say = |phase: u8, label: &str, done: u64, total: u64| {
        progress(Progress {
            phase,
            label: label.into(),
            done,
            total,
        })
    };

    say(1, "Checking the new folder…", 0, 1);
    let b = storage::read_bootstrap().ok_or(anyhow!("Not set up yet"))?;
    if b.migration_state != "IDLE" {
        bail!("A previous move hasn't been finished. Please restart Graphite Mail.");
    }
    let source = normalize(Path::new(&b.active_data_root))?;
    let dest = validate(&source, Path::new(target))?;
    checkpoint(1)?;

    say(2, "Getting ready…", 0, 1);
    let dest_s = dest.to_string_lossy().to_string();
    set_state("COPYING", Some(&dest_s), None)?;

    match copy_and_switch(&source, &dest, &say) {
        Ok(()) => {}
        Err(e) if e.downcast_ref::<Crash>().is_some() => return Err(e), // test only: leave things as a crash would
        Err(e) => {
            // Undo everything: the old folder was never touched, and the old database is still open.
            remove_own(&dest);
            let _ = set_state("IDLE", None, None);
            return Err(e);
        }
    }

    // Phase 7
    say(7, "Cleaning up the old folder…", 0, 1);
    finish_cleanup(&source);
    say(7, "Done", 1, 1);
    Ok(dest_s)
}

fn finish_cleanup(old: &Path) {
    remove_own(old);
    let _ = set_state("IDLE", None, None);
}

fn validate(source: &Path, target: &Path) -> Result<PathBuf> {
    if !target.is_absolute() {
        bail!("Please choose a full folder path.");
    }
    let dest = normalize(target)?;
    if dest == source || dest.starts_with(source) || source.starts_with(&dest) {
        bail!("Please choose a folder outside your current data folder.");
    }
    if dest.exists() {
        if !dest.is_dir() {
            bail!("That is a file, not a folder.");
        }
        if fs::read_dir(&dest)?.next().is_some() {
            bail!("That folder isn't empty. Please choose a new or empty folder.");
        }
    }
    fs::create_dir_all(&dest).with_context(|| format!("Cannot create {}", dest.display()))?;
    let probe = dest.join(".write_test");
    fs::write(&probe, b"ok").context("That folder is not writable.")?;
    fs::remove_file(probe)?;
    let need = dir_size(source) / 10 * 11 + (50 << 20);
    if let Some(free) = free_space(&dest) {
        if free < need {
            let _ = fs::remove_dir(&dest);
            bail!(
                "Not enough free space there: about {} MB needed, {} MB free.",
                need >> 20,
                free >> 20
            );
        }
    }
    Ok(dest)
}

fn copy_and_switch(source: &Path, dest: &Path, say: &dyn Fn(u8, &str, u64, u64)) -> Result<()> {
    let stage = dest.join(STAGING);
    storage::private_dir(&stage.join("blobs"))?;

    // Phase 3: bulk copy of saved mail while the app keeps working.
    let first = list_blobs(source);
    let total = first.len() as u64 + 1;
    for (i, rel) in first.iter().enumerate() {
        copy_verified(
            &source.join("blobs").join(rel),
            &stage.join("blobs").join(rel),
        )?;
        if i % 20 == 0 {
            say(3, "Copying saved mail…", i as u64, total);
        }
    }
    checkpoint(3)?;

    // Phase 4: short exclusive window. Anything saved since phase 3 is copied, then the database is snapshotted.
    say(4, "Copying the database…", first.len() as u64, total);
    let mut guard = storage::DB.lock().unwrap();
    let db = guard.as_ref().ok_or(anyhow!("Database not open"))?;
    for rel in list_blobs(source) {
        let to = stage.join("blobs").join(&rel);
        if !to.exists() {
            copy_verified(&source.join("blobs").join(&rel), &to)?;
        }
    }
    let _ = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    storage::private_dir(&stage.join("db"))?;
    let snap = stage.join("db").join("mail_core.sqlite");
    db.execute("VACUUM INTO ?1", [snap.to_string_lossy().as_ref()])?;
    storage::make_private(&snap);
    storage::private_dir(&stage.join("temp_staging"))?;
    checkpoint(4)?;

    // Phase 5: check the copy before relying on it.
    say(5, "Checking the copy…", 0, 1);
    {
        let copy = rusqlite::Connection::open(&snap)?;
        let ok: String = copy.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if ok != "ok" {
            bail!("The copy of the database did not pass its safety check. Nothing was changed.");
        }
        for t in TABLES {
            let q = format!("SELECT COUNT(*) FROM {t}");
            let (a, b): (i64, i64) = (
                db.query_row(&q, [], |r| r.get(0))?,
                copy.query_row(&q, [], |r| r.get(0))?,
            );
            if a != b {
                bail!("The copy is missing some {t} ({a} vs {b}). Nothing was changed.");
            }
        }
    }
    for rel in list_blobs(source) {
        let (a, b) = (
            fs::metadata(source.join("blobs").join(&rel))?.len(),
            fs::metadata(stage.join("blobs").join(&rel))
                .map(|m| m.len())
                .unwrap_or(u64::MAX),
        );
        if a != b {
            bail!("A saved message file is missing from the copy. Nothing was changed.");
        }
    }
    checkpoint(5)?;

    // Phase 6: move into place, prove it opens, then flip the pointer in one atomic step.
    say(6, "Switching to the new folder…", 0, 1);
    for d in OWN_DIRS {
        fs::rename(stage.join(d), dest.join(d))?;
    }
    fs::remove_dir(&stage)?;
    sync_dir(dest);
    let reopened = storage::connect(dest).context("The new folder could not be opened")?;
    checkpoint(6)?;
    let mut b: Bootstrap = storage::read_bootstrap().ok_or(anyhow!("Not set up yet"))?;
    let (old, new) = (
        b.active_data_root.clone(),
        dest.to_string_lossy().to_string(),
    );
    b.active_data_root = new;
    b.migration_state = "SWITCHED".into();
    b.pending_migration_target = None;
    b.previous_data_root = Some(old);
    storage::write_bootstrap(&b)?; // <- the point of no return
    *guard = Some(reopened);
    drop(guard);
    checkpoint(7)?;
    Ok(())
}

/// Start-up: finish or undo a move that was interrupted. Safe to run any time; does nothing when idle.
pub fn recover() -> Result<()> {
    let Some(b) = storage::read_bootstrap() else {
        return Ok(());
    };
    match b.migration_state.as_str() {
        // Died before the switch: the old folder is still the real one. Throw the half-made copy away.
        "COPYING" => {
            if let Some(t) = &b.pending_migration_target {
                let t = Path::new(t);
                if normalize(t).ok() != normalize(Path::new(&b.active_data_root)).ok() {
                    remove_own(t);
                }
            }
            set_state("IDLE", None, None)?;
        }
        // Died after the switch: the new folder is the real one. Finish removing the old one.
        "SWITCHED" => {
            if let Some(old) = &b.previous_data_root {
                if Path::new(&b.active_data_root).join("db").exists() {
                    finish_cleanup(Path::new(old));
                    return Ok(());
                }
            }
            set_state("IDLE", None, None)?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::SeqCst;

    /// A vault with a database and some saved mail, in a fresh temp area.
    fn setup() -> (PathBuf, PathBuf) {
        let tmp = std::env::temp_dir().join(format!("gm-move-{}", uuid::Uuid::new_v4()));
        unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.join("cfg")) };
        let vault = tmp.join("old").join("graphite-mail");
        storage::initialize(vault.to_str().unwrap()).unwrap();
        let hash = crate::sync::write_blob(b"<p>hello</p>", "html").unwrap();
        crate::sync::write_blob(&vec![7u8; 3_000_000], "bin").unwrap();
        storage::with_db(|db| {
            db.execute_batch(&format!(
                "INSERT INTO accounts(id,email,display_name,created_at) VALUES('a','me@x.com','Me',0);
                 INSERT INTO folders(id,account_id,remote_id,display_name) VALUES('f','a','rf','Inbox');
                 INSERT INTO messages(id,account_id,folder_id,remote_id,subject,received_at,body_blob_hash,body_cached) VALUES('m1','a','f','r1','Budget',1,'{hash}',1);"
            ))?;
            Ok(())
        })
        .unwrap();
        (tmp, vault)
    }

    fn assert_works_at(root: &Path) {
        let b = storage::read_bootstrap().unwrap();
        // The pointer keeps the path as it was given (on Windows that can be a short "RUNNER~1" form), so compare
        // canonical forms; and whatever it holds must never carry Windows' verbatim "\\?\" prefix.
        assert_eq!(dunce::canonicalize(&b.active_data_root).unwrap(), dunce::canonicalize(root).unwrap());
        assert!(!b.active_data_root.starts_with("\\\\?\\"), "{}", b.active_data_root);
        assert_eq!(b.migration_state, "IDLE");
        storage::with_db(|db| {
            let n: i64 = db.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
            assert_eq!(n, 1);
            Ok(())
        })
        .unwrap();
        let msg = crate::mail::message("m1").unwrap();
        assert_eq!(msg.body_html.as_deref(), Some("<p>hello</p>"));
        assert!(root.join("temp_staging").exists());
    }

    /// Every path the app keeps or shows (the data folder pointer, the pending target, the moved-to folder) is a plain
    /// absolute path: on Windows `C:\Users\…`, never the `\\?\` verbatim form `std::fs::canonicalize` produces.
    #[test]
    fn stored_and_shown_paths_never_carry_the_verbatim_prefix() {
        let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        TEST_STOP.store(0, SeqCst);
        let (tmp, _vault) = setup();
        let n = normalize(&tmp.join("not-yet").join("there")).unwrap();
        assert!(n.is_absolute() && !n.to_string_lossy().starts_with("\\\\?\\"), "{n:?}");
        let out = run(tmp.join("moved").to_str().unwrap(), |_| {}).unwrap();
        assert!(!out.starts_with("\\\\?\\"), "{out}");
        let b = storage::read_bootstrap().unwrap();
        assert!(!b.active_data_root.starts_with("\\\\?\\") && b.previous_data_root.is_none());
        assert_eq!(info().unwrap().path, out, "Settings shows the same plain path");
        // A second move still recognises "inside the current folder" and "same folder" after the switch.
        assert!(run(Path::new(&out).join("inner").to_str().unwrap(), |_| {}).unwrap_err().to_string().contains("outside"));
        assert!(run(&out, |_| {}).unwrap_err().to_string().contains("outside"));
    }

    #[test]
    fn moves_everything_and_removes_the_old_copy() {
        let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        TEST_STOP.store(0, SeqCst);
        let (tmp, vault) = setup();
        let target = tmp.join("new").join("mail-here");
        let phases = std::sync::Mutex::new(Vec::new());
        let out = run(target.to_str().unwrap(), |p| {
            phases.lock().unwrap().push(p.phase)
        })
        .unwrap();
        let target = dunce::canonicalize(&target).unwrap();
        assert_eq!(Path::new(&out), target, "the path handed back to the UI is canonical but has no verbatim prefix");
        assert_works_at(&target);
        assert!(
            !vault.join("db").exists() && !vault.join("blobs").exists(),
            "old copy should be gone"
        );
        let seen: std::collections::BTreeSet<u8> = phases.lock().unwrap().iter().copied().collect();
        assert_eq!(
            seen.into_iter().collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(list_blobs(&target).len(), 2);
        // a second move works too (back again)
        run(vault.to_str().unwrap(), |_| {}).unwrap();
        assert_works_at(&dunce::canonicalize(&vault).unwrap());
    }

    #[test]
    fn a_failure_in_any_early_phase_changes_nothing() {
        let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (tmp, vault) = setup();
        for phase in [1u8, 3, 4, 5, 6] {
            let target = tmp.join(format!("new-{phase}"));
            TEST_STOP.store(phase, SeqCst);
            let err = run(target.to_str().unwrap(), |_| {}).unwrap_err();
            assert!(err.to_string().contains("injected"), "phase {phase}: {err}");
            TEST_STOP.store(0, SeqCst);
            assert_works_at(&dunce::canonicalize(&vault).unwrap());
            assert!(
                !target.join("db").exists() && !target.join(STAGING).exists(),
                "phase {phase}: leftovers"
            );
        }
    }

    #[test]
    fn a_crash_before_the_switch_is_undone_on_restart() {
        let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (tmp, vault) = setup();
        for phase in [3u8, 4, 5, 6] {
            let target = tmp.join(format!("crash-{phase}"));
            TEST_STOP.store(phase | CRASH, SeqCst);
            run(target.to_str().unwrap(), |_| {}).unwrap_err();
            TEST_STOP.store(0, SeqCst);
            assert_eq!(
                storage::read_bootstrap().unwrap().migration_state,
                "COPYING"
            );
            // "restart"
            assert!(storage::open_existing().unwrap());
            assert_works_at(&dunce::canonicalize(&vault).unwrap());
            assert!(
                !target.exists() || fs::read_dir(&target).unwrap().next().is_none(),
                "phase {phase}: copy not cleaned"
            );
        }
    }

    #[test]
    fn a_crash_after_the_switch_is_finished_on_restart() {
        let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (tmp, vault) = setup();
        let target = tmp.join("moved");
        TEST_STOP.store(7 | CRASH, SeqCst);
        run(target.to_str().unwrap(), |_| {}).unwrap_err();
        TEST_STOP.store(0, SeqCst);
        let b = storage::read_bootstrap().unwrap();
        assert_eq!(b.migration_state, "SWITCHED");
        assert!(
            vault.join("db").exists(),
            "old copy still there at the moment of the crash"
        );
        assert!(storage::open_existing().unwrap()); // "restart"
        assert_works_at(&dunce::canonicalize(&target).unwrap());
        assert!(!vault.join("db").exists(), "old copy removed by recovery");
    }

    #[test]
    fn refuses_bad_targets() {
        let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        TEST_STOP.store(0, SeqCst);
        let (tmp, vault) = setup();
        for bad in [
            vault.clone(),
            vault.join("inside"),
            vault.parent().unwrap().to_path_buf(),
        ] {
            assert!(
                run(bad.to_str().unwrap(), |_| {}).is_err(),
                "{}",
                bad.display()
            );
        }
        let full = tmp.join("full");
        fs::create_dir_all(&full).unwrap();
        fs::write(full.join("keep.txt"), "mine").unwrap();
        assert!(
            run(full.to_str().unwrap(), |_| {})
                .unwrap_err()
                .to_string()
                .contains("isn't empty")
        );
        assert!(run("relative/path", |_| {}).is_err());
        assert!(
            full.join("keep.txt").exists(),
            "user files are never touched"
        );
        assert_works_at(&dunce::canonicalize(&vault).unwrap());
    }

    #[tokio::test]
    async fn works_the_way_the_app_calls_it() {
        // The app runs the move on a background thread of the async runtime, while a sync may be running.
        let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        TEST_STOP.store(0, SeqCst);
        let (tmp, _vault) = setup();
        let target = tmp.join("from-app");
        let busy = crate::sync::SYNC_LOCK.lock().await; // pretend a sync is running
        let t = target.to_string_lossy().to_string();
        let job = tokio::task::spawn_blocking(move || run(&t, |_| {}));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !job.is_finished(),
            "the move must wait for the running sync"
        );
        drop(busy);
        job.await.unwrap().unwrap();
        assert_works_at(&dunce::canonicalize(&target).unwrap());
    }
}
