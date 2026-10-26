//! Split storage: a fixed bootstrap pointer in the per-user config folder (`~/.config/graphite-mail/bootstrap.json`
//! on Linux, `%APPDATA%\graphite-mail\bootstrap.json` on Windows) and a user-chosen, movable data vault holding the
//! SQLite DB and blob store.

use anyhow::{Context, Result, anyhow};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Bootstrap {
    pub schema_version: u32,
    pub active_data_root: String,
    pub migration_state: String,
    pub pending_migration_target: Option<String>,
    /// While a finished move is being tidied up: the folder the data came from.
    #[serde(default)]
    pub previous_data_root: Option<String>,
}

pub(crate) static DB: Mutex<Option<Connection>> = Mutex::new(None);

fn config_dir() -> Result<PathBuf> {
    // XDG_CONFIG_HOME wins on every platform. Linux honours it anyway; on Windows it lets the tests keep their
    // bootstrap.json in a scratch folder instead of the real %APPDATA%.
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Ok(PathBuf::from(x).join("graphite-mail"));
    }
    Ok(dirs::config_dir()
        .ok_or(anyhow!("No config dir"))?
        .join("graphite-mail"))
}

fn bootstrap_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("bootstrap.json"))
}

/// Where mail goes unless the user picks another place: `~/.local/share/graphite-mail` on Linux,
/// `%LOCALAPPDATA%\graphite-mail` on Windows (local, not roaming: a mailbox can be gigabytes).
/// In the snap (Linux only): `~/snap/graphite-mail/common/graphite-mail`.
pub fn default_data_dir() -> String {
    #[cfg(target_os = "linux")]
    {
        default_data_dir_in(std::env::var_os("SNAP_USER_COMMON").map(PathBuf::from), dirs::data_local_dir())
    }
    #[cfg(not(target_os = "linux"))]
    {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("graphite-mail")
            .to_string_lossy()
            .into()
    }
}

/// The snap's `$SNAP_USER_COMMON` (`~/snap/graphite-mail/common`) wins over the platform's data folder: snapd sets
/// `$HOME` and `XDG_DATA_HOME` to the per-revision folder (`~/snap/graphite-mail/<revision>`), which it copies on
/// every update and deletes with old revisions. A mailbox of several gigabytes must stay in one place.
/// Snaps exist only on Linux, so this (and its test) is Linux-only.
#[cfg(target_os = "linux")]
fn default_data_dir_in(snap_common: Option<PathBuf>, data_local: Option<PathBuf>) -> String {
    snap_common
        .filter(|p| p.is_absolute())
        .or(data_local)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("graphite-mail")
        .to_string_lossy()
        .into()
}

pub fn read_bootstrap() -> Option<Bootstrap> {
    let s = fs::read_to_string(bootstrap_path().ok()?).ok()?;
    serde_json::from_str(&s).ok()
}

/// Atomic write: temp file + fsync + rename + fsync parent directory.
pub(crate) fn write_bootstrap(b: &Bootstrap) -> Result<()> {
    let dir = config_dir()?;
    private_dir(&dir)?;
    let tmp = dir.join("bootstrap.json.tmp");
    {
        let f = private_file(&tmp)?;
        serde_json::to_writer_pretty(&f, b)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, bootstrap_path()?)?;
    sync_dir(&dir)?;
    Ok(())
}

/// Makes a directory's entries (a just-renamed or just-written file) durable.
/// Unix: open the directory and fsync it. Windows: `File::open` on a directory is refused ("Access is denied",
/// os error 5: directories need FILE_FLAG_BACKUP_SEMANTICS), and NTFS journals metadata changes itself, so there is
/// nothing to do. Before v1.11.1 this call made the first launch on Windows fail.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// The app's own folders inside the data folder (only these are made private; the folder itself may be shared).
pub(crate) const OWN_DIRS: [&str; 4] = ["db", "blobs", "temp_staging", ".graphite-moving"];

/// Creates a folder (and missing parents) that only the user can open: 0700 on Linux. Other systems: plain create.
pub(crate) fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

/// Creates (or empties) a file only the user can read: 0600 on Linux.
pub(crate) fn private_file(path: &Path) -> std::io::Result<fs::File> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        o.mode(0o600);
        let f = o.open(path)?;
        f.set_permissions(fs::Permissions::from_mode(0o600))?; // an existing file keeps its old mode otherwise
        Ok(f)
    }
    #[cfg(not(unix))]
    {
        o.open(path)
    }
}

/// Linux: makes a tree private (folders 0700, files 0600), without following links. Fixes folders made by older
/// versions (0755/0644). Returns how many entries were changed. Other systems: nothing to do (per-user profile ACLs).
pub(crate) fn make_private(path: &Path) -> usize {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = fs::symlink_metadata(path) else { return 0 };
        if meta.file_type().is_symlink() {
            return 0;
        }
        let want = if meta.is_dir() { 0o700 } else { 0o600 };
        let mut changed = 0;
        if meta.permissions().mode() & 0o7777 != want && fs::set_permissions(path, fs::Permissions::from_mode(want)).is_ok() {
            changed += 1;
        }
        if meta.is_dir() {
            if let Ok(rd) = fs::read_dir(path) {
                for e in rd.filter_map(|e| e.ok()) {
                    changed += make_private(&e.path());
                }
            }
        }
        changed
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0
    }
}

/// Makes the data folder, the app's folders and files in it, and the settings folder private. Run at every start and
/// whenever a data folder is opened, so folders created by older versions are fixed.
pub(crate) fn secure(root: &Path) {
    let mut changed = 0;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::symlink_metadata(root).is_ok_and(|m| m.is_dir() && m.permissions().mode() & 0o777 != 0o700) && fs::set_permissions(root, fs::Permissions::from_mode(0o700)).is_ok() {
            changed += 1;
        }
    }
    for d in OWN_DIRS {
        changed += make_private(&root.join(d));
    }
    if let Ok(cfg) = config_dir() {
        changed += make_private(&cfg);
    }
    if changed > 0 {
        eprintln!("[graphite] made {changed} files and folders private");
    }
}

/// First-run: validate the chosen folder, create the vault layout and DB, then record the pointer.
pub fn initialize(data_root: &str) -> Result<()> {
    let root = Path::new(data_root);
    fs::create_dir_all(root).with_context(|| format!("Cannot create {data_root}"))?;
    let probe = root.join(".write_test");
    fs::write(&probe, b"ok").context("Folder is not writable")?;
    fs::remove_file(probe)?;
    for d in ["db", "blobs", "temp_staging"] {
        private_dir(&root.join(d))?;
    }
    open_db(root)?;
    write_bootstrap(&Bootstrap {
        schema_version: 1,
        active_data_root: data_root.into(),
        migration_state: "IDLE".into(),
        pending_migration_target: None,
        previous_data_root: None,
    })
}

/// Opens the DB from bootstrap on app start. Returns false when first-run setup is needed.
pub fn open_existing() -> Result<bool> {
    // A move that was interrupted (power cut, crash) is finished or undone first.
    crate::migrate::recover()?;
    match read_bootstrap() {
        Some(b) if Path::new(&b.active_data_root).join("db").exists() => {
            open_db(Path::new(&b.active_data_root))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) fn connect(root: &Path) -> Result<Connection> {
    let conn = Connection::open(root.join("db").join("mail_core.sqlite"))?;
    conn.execute_batch(SCHEMA)?;
    migrate(&conn)?;
    // The database (and its -wal/-shm files, which SQLite creates with the database's mode) and the mail are private.
    secure(root);
    Ok(conn)
}

fn open_db(root: &Path) -> Result<()> {
    *DB.lock().unwrap() = Some(connect(root)?);
    Ok(())
}

/// Small in-place upgrades for databases created by older versions.
fn migrate(conn: &Connection) -> Result<()> {
    let has = |table: &str, col: &str| -> Result<bool> {
        let mut st = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let names: Vec<String> = st.query_map([], |r| r.get(1))?.collect::<Result<_, _>>()?;
        Ok(names.iter().any(|n| n == col))
    };
    if !has("messages", "inline_done")? {
        // 0 = embedded pictures not fetched yet (checked on the next sync).
        conn.execute_batch("ALTER TABLE messages ADD COLUMN inline_done INTEGER DEFAULT 0")?;
    }
    if !has("accounts", "provider")? {
        conn.execute_batch("ALTER TABLE accounts ADD COLUMN provider TEXT DEFAULT 'microsoft'")?;
    }
    if !has("offline_mutation_queue", "failed")? {
        // 1 = the server turned this change down; it stays listed so nothing is lost silently.
        conn.execute_batch(
            "ALTER TABLE offline_mutation_queue ADD COLUMN failed INTEGER DEFAULT 0",
        )?;
    }
    if !has("contacts", "given_name")? {
        // v1.10.1: name parts for vCard export (filled in by the next contacts sync).
        conn.execute_batch("ALTER TABLE contacts ADD COLUMN given_name TEXT; ALTER TABLE contacts ADD COLUMN surname TEXT;")?;
    }
    if !has("contacts", "message_count")? {
        // v1.11.0: "People you've emailed" for Gmail (app password) accounts.
        conn.execute_batch("ALTER TABLE contacts ADD COLUMN message_count INTEGER NOT NULL DEFAULT 0; ALTER TABLE contacts ADD COLUMN last_contacted INTEGER;")?;
    }
    if !has("drafts", "bcc_text")? {
        // v1.10.3: blind copies in drafts.
        conn.execute_batch("ALTER TABLE drafts ADD COLUMN bcc_text TEXT NOT NULL DEFAULT ''")?;
    }
    Ok(())
}

/// Tests share one global database, so they take turns.
#[cfg(test)]
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

pub fn with_db<T>(f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let g = DB.lock().unwrap();
    f(g.as_ref().ok_or(anyhow!("Database not open"))?)
}

pub fn blob_dir() -> Result<PathBuf> {
    Ok(Path::new(
        &read_bootstrap()
            .ok_or(anyhow!("Not set up"))?
            .active_data_root,
    )
    .join("blobs"))
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS accounts (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    display_name TEXT,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS folders (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_id TEXT NOT NULL,
    parent_remote_id TEXT,
    display_name TEXT NOT NULL,
    well_known_name TEXT,
    delta_link TEXT,
    unread_count INTEGER DEFAULT 0,
    total_count INTEGER DEFAULT 0,
    UNIQUE(account_id, remote_id)
);

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    folder_id TEXT NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    remote_id TEXT UNIQUE,
    internet_message_id TEXT,
    conversation_id TEXT,
    subject TEXT,
    sender_name TEXT,
    sender_address TEXT,
    to_recipients_json TEXT,
    cc_recipients_json TEXT,
    snippet TEXT,
    received_at INTEGER NOT NULL,
    is_read INTEGER DEFAULT 0,
    is_flagged INTEGER DEFAULT 0,
    is_draft INTEGER DEFAULT 0,
    has_attachments INTEGER DEFAULT 0,
    body_blob_hash TEXT,
    body_cached INTEGER DEFAULT 0,
    change_key TEXT,
    sync_status TEXT DEFAULT 'SYNCED'
);
CREATE INDEX IF NOT EXISTS idx_messages_folder_date ON messages(folder_id, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_messages_conversation ON messages(account_id, conversation_id);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    message_id UNINDEXED, subject, sender_name, sender_address, body_plaintext,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TABLE IF NOT EXISTS attachments (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    remote_attachment_id TEXT,
    filename TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    content_id TEXT,
    is_inline INTEGER DEFAULT 0,
    blob_hash TEXT,
    is_downloaded INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS calendars (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    remote_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    color_hex TEXT NOT NULL,
    is_primary INTEGER DEFAULT 0,
    can_edit INTEGER DEFAULT 1,
    delta_link TEXT
);

CREATE TABLE IF NOT EXISTS calendar_events (
    id TEXT PRIMARY KEY,
    calendar_id TEXT NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
    remote_id TEXT UNIQUE,
    ical_uid TEXT NOT NULL,
    series_master_id TEXT,
    event_type TEXT NOT NULL,
    title TEXT NOT NULL,
    description_html TEXT,
    location TEXT,
    online_meeting_url TEXT,
    start_utc INTEGER NOT NULL,
    end_utc INTEGER NOT NULL,
    original_timezone TEXT NOT NULL,
    is_all_day INTEGER DEFAULT 0,
    organizer_json TEXT,
    attendees_json TEXT,
    my_response_status TEXT DEFAULT 'NONE',
    change_key TEXT,
    sync_status TEXT DEFAULT 'SYNCED'
);
CREATE INDEX IF NOT EXISTS idx_calendar_events_range ON calendar_events(calendar_id, start_utc, end_utc);

-- Gmail: a message can carry several labels (folders); messages.folder_id is only its primary one.
CREATE TABLE IF NOT EXISTS message_labels (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    folder_id TEXT NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    PRIMARY KEY (message_id, folder_id)
);
CREATE INDEX IF NOT EXISTS idx_message_labels_folder ON message_labels(folder_id);

-- IMAP accounts: every (folder, UID) a message is stored under on the server. message_id has no foreign key
-- on purpose: after a message is deleted here, its location is still needed to delete it on the server.
CREATE TABLE IF NOT EXISTS imap_uids (
    folder_id TEXT NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    uid INTEGER NOT NULL,
    validity INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    size INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (folder_id, uid)
);
CREATE INDEX IF NOT EXISTS idx_imap_uids_message ON imap_uids(message_id);

-- Microsoft 365 contacts, kept on this computer only: the organisation's directory and the user's own contacts.
CREATE TABLE IF NOT EXISTS contacts (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    source TEXT NOT NULL,              -- 'directory', 'personal' (Microsoft 365), 'emailed', 'imported' (Gmail app password)
    remote_id TEXT NOT NULL,
    folder_id TEXT,                    -- personal: the contact folder it is in
    folder_name TEXT,
    display_name TEXT NOT NULL,
    given_name TEXT,
    surname TEXT,
    emails_json TEXT NOT NULL DEFAULT '[]',
    phones_json TEXT NOT NULL DEFAULT '[]',
    job_title TEXT,
    department TEXT,
    office TEXT,
    company TEXT,
    search_text TEXT NOT NULL DEFAULT '',  -- lower-case name, emails, title, department for searching
    sync_gen INTEGER NOT NULL DEFAULT 0,   -- full listings: rows not seen in the latest one are removed
    message_count INTEGER NOT NULL DEFAULT 0, -- "people you've emailed": messages exchanged with them
    last_contacted INTEGER,                -- "people you've emailed": when the latest of those arrived
    UNIQUE(account_id, source, remote_id)
);
CREATE INDEX IF NOT EXISTS idx_contacts_list ON contacts(account_id, source, display_name);

-- Contacts marked as favourites (by their Graph id, so they survive a refresh). Go with the account.
CREATE TABLE IF NOT EXISTS contact_favourites (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    source TEXT NOT NULL,
    remote_id TEXT NOT NULL,
    PRIMARY KEY (account_id, source, remote_id)
);

-- Per-account sync bookmarks (e.g. Gmail history id).
CREATE TABLE IF NOT EXISTS account_state (
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (account_id, key)
);

-- Unfinished messages (kept on this computer only).
CREATE TABLE IF NOT EXISTS drafts (
    id TEXT PRIMARY KEY,
    mode TEXT NOT NULL DEFAULT 'new',
    reply_to TEXT,
    from_email TEXT,
    to_text TEXT NOT NULL DEFAULT '',
    cc_text TEXT NOT NULL DEFAULT '',
    bcc_text TEXT NOT NULL DEFAULT '',
    subject TEXT NOT NULL DEFAULT '',
    html TEXT NOT NULL DEFAULT '',
    attachments_json TEXT NOT NULL DEFAULT '[]',
    updated_at INTEGER NOT NULL
);

-- Mail rules (kept on the server as Outlook rules / Gmail filters; mirrored here).
CREATE TABLE IF NOT EXISTS rules (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    from_text TEXT NOT NULL DEFAULT '',
    subject_text TEXT NOT NULL DEFAULT '',
    to_text TEXT NOT NULL DEFAULT '',
    body_text TEXT NOT NULL DEFAULT '',
    move_to_folder TEXT REFERENCES folders(id) ON DELETE SET NULL,
    mark_read INTEGER NOT NULL DEFAULT 0,
    flag INTEGER NOT NULL DEFAULT 0,
    del INTEGER NOT NULL DEFAULT 0,
    remote_id TEXT,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS offline_mutation_queue (
    seq_id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    entity_type TEXT NOT NULL,
    local_entity_id TEXT NOT NULL,
    operation TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    retry_count INTEGER DEFAULT 0,
    next_retry_at INTEGER DEFAULT 0,
    last_error TEXT,
    created_at INTEGER NOT NULL
);
"#;

#[cfg(test)]
mod tests {
    /// Snaps exist only on Linux (and the paths here are Linux paths).
    #[cfg(target_os = "linux")]
    #[test]
    fn in_the_snap_the_default_data_folder_is_the_common_folder_that_survives_updates() {
        use std::path::PathBuf;
        let local = Some(PathBuf::from("/home/ann/.local/share"));
        assert_eq!(super::default_data_dir_in(None, local.clone()), "/home/ann/.local/share/graphite-mail");
        assert_eq!(super::default_data_dir_in(Some(PathBuf::from("/home/ann/snap/graphite-mail/common")), local.clone()), "/home/ann/snap/graphite-mail/common/graphite-mail");
        // an empty or relative SNAP_USER_COMMON is ignored
        assert_eq!(super::default_data_dir_in(Some(PathBuf::from("")), local.clone()), super::default_data_dir_in(None, local.clone()));
        assert_eq!(super::default_data_dir_in(Some(PathBuf::from("common")), local.clone()), super::default_data_dir_in(None, local));
    }

    #[test]
    fn the_private_folders_are_the_ones_the_app_owns() {
        assert_eq!(super::OWN_DIRS, ["db", "blobs", "temp_staging", ".graphite-moving"], "keep in step with migrate.rs OWN_DIRS and STAGING");
    }

    /// Linux: a fresh data folder is private, and one made by an older version (0755 folders, 0644 files) is fixed
    /// when the app starts; links are never followed and the user's other files are left alone.
    #[cfg(unix)]
    #[test]
    fn data_and_settings_are_private_and_old_folders_are_fixed_at_start() {
        use std::os::unix::fs::PermissionsExt;
        let _g = super::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mode = |p: &std::path::Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777;
        let set = |p: &std::path::Path, m: u32| std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
        let tmp = std::env::temp_dir().join(format!("gm-perm-{}", uuid::Uuid::new_v4()));
        unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.join("cfg")) };
        let vault = tmp.join("vault");
        super::initialize(vault.to_str().unwrap()).unwrap();
        let hash = crate::sync::write_blob(b"<p>mail</p>", "html").unwrap();
        let blob = vault.join("blobs").join(&hash[..2]).join(format!("{hash}.html"));
        let cfg = tmp.join("cfg").join("graphite-mail");
        let db = vault.join("db").join("mail_core.sqlite");
        for (p, m) in [(&vault, 0o700), (&vault.join("db"), 0o700), (&vault.join("blobs"), 0o700), (&blob.parent().unwrap().to_path_buf(), 0o700), (&blob, 0o600), (&db, 0o600), (&cfg, 0o700), (&cfg.join("bootstrap.json"), 0o600)] {
            assert_eq!(mode(p), m, "{}", p.display());
        }

        // As an older version left it: everything readable by others; plus a link to a file outside, and a user file.
        for d in [&vault, &vault.join("db"), &vault.join("blobs"), &blob.parent().unwrap().to_path_buf(), &cfg] {
            set(d, 0o755);
        }
        for f in [&blob, &db, &cfg.join("bootstrap.json")] {
            set(f, 0o644);
        }
        let outside = tmp.join("outside.txt");
        std::fs::write(&outside, b"x").unwrap();
        set(&outside, 0o644);
        std::os::unix::fs::symlink(&outside, vault.join("blobs").join("link")).unwrap();
        std::fs::write(vault.join("my-notes.txt"), b"mine").unwrap();
        set(&vault.join("my-notes.txt"), 0o644);

        *super::DB.lock().unwrap() = None;
        assert!(super::open_existing().unwrap(), "start-up opens the existing data folder");
        for (p, m) in [(&vault, 0o700), (&vault.join("db"), 0o700), (&vault.join("blobs"), 0o700), (&blob.parent().unwrap().to_path_buf(), 0o700), (&blob, 0o600), (&db, 0o600), (&cfg, 0o700), (&cfg.join("bootstrap.json"), 0o600)] {
            assert_eq!(mode(p), m, "fixed at start: {}", p.display());
        }
        assert_eq!(mode(&outside), 0o644, "links are not followed");
        assert_eq!(mode(&vault.join("my-notes.txt")), 0o644, "only the app's own folders are changed");
        // WAL files SQLite creates later get the database's mode.
        super::with_db(|db| Ok(db.execute_batch("CREATE TABLE IF NOT EXISTS t(x); INSERT INTO t VALUES(1);")?)).unwrap();
        for side in ["mail_core.sqlite-wal", "mail_core.sqlite-shm"] {
            let p = vault.join("db").join(side);
            if p.exists() {
                assert_eq!(mode(&p), 0o600, "{side}");
            }
        }
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn schema_applies() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(super::SCHEMA).unwrap();
        c.execute_batch(super::SCHEMA).unwrap(); // idempotent
    }

    #[test]
    fn a_v1_10_0_contacts_table_gets_name_columns() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        // The contacts table as v1.10.0 created it (no name parts).
        c.execute_batch(&super::SCHEMA.replace("    given_name TEXT,\n    surname TEXT,\n", "")).unwrap();
        c.execute_batch("INSERT INTO accounts(id,email,created_at) VALUES('a','me@x.com',0); INSERT INTO contacts(id,account_id,source,remote_id,display_name) VALUES('c','a','personal','r','Ana');").unwrap();
        super::migrate(&c).unwrap();
        let (g, s): (Option<String>, Option<String>) = c.query_row("SELECT given_name, surname FROM contacts", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((g, s), (None, None), "kept the contact, added the columns");
        super::migrate(&c).unwrap(); // and again: nothing to do
    }

    #[test]
    fn a_v1_10_2_drafts_table_gets_a_bcc_column() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(&super::SCHEMA.replace("    bcc_text TEXT NOT NULL DEFAULT '',\n", "")).unwrap();
        c.execute_batch("INSERT INTO drafts(id,to_text,updated_at) VALUES('d','a@b.c',1)").unwrap();
        super::migrate(&c).unwrap();
        let bcc: String = c.query_row("SELECT bcc_text FROM drafts WHERE id='d'", [], |r| r.get(0)).unwrap();
        assert_eq!(bcc, "", "kept the draft, added the column");
        super::migrate(&c).unwrap();
    }
}
