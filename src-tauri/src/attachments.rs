//! Attachments: list a message's files, fetch them when asked (and keep them for offline), open or save them,
//! and prepare files chosen in the editor for sending.

use crate::{auth, google, storage, sync};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use reqwest::Method;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
/// All files of one outgoing message together (Gmail's limit is 25 MB including encoding overhead).
pub const MAX_OUTGOING: u64 = 15 * 1024 * 1024;

fn graph() -> String {
    #[cfg(test)]
    if let Ok(u) = std::env::var("GM_TEST_GRAPH") {
        return u;
    }
    GRAPH.to_string()
}

#[derive(Serialize)]
pub struct AttachmentRow {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub downloaded: bool,
}

struct Owner {
    email: String,
    provider: String,
    remote_id: Option<String>,
    has_attachments: bool,
}

fn owner_of_message(message_id: &str) -> Result<Owner> {
    storage::with_db(|db| {
        db.query_row(
            "SELECT a.email, COALESCE(a.provider,'microsoft'), m.remote_id, m.has_attachments FROM messages m JOIN accounts a ON a.id=m.account_id WHERE m.id=?1",
            [message_id],
            |r| Ok(Owner { email: r.get(0)?, provider: r.get(1)?, remote_id: r.get(2)?, has_attachments: r.get(3)? }),
        )
        .optional()?
        .ok_or(anyhow!("That message no longer exists."))
    })
}

fn rows(message_id: &str) -> Result<Vec<AttachmentRow>> {
    storage::with_db(|db| {
        let mut st = db.prepare("SELECT id, filename, mime_type, size_bytes, is_downloaded FROM attachments WHERE message_id=?1 AND is_inline=0 ORDER BY rowid")?;
        // Names are shown (and offered in the save dialog) the way they would be written.
        let r = st.query_map([message_id], |r| Ok(AttachmentRow { id: r.get(0)?, name: safe_name(&r.get::<_, String>(1)?), mime: r.get(2)?, size: r.get(3)?, downloaded: r.get(4)? }))?;
        Ok(r.filter_map(|x| x.ok()).collect())
    })
}

/// Outlook: records the names and sizes of a message's real attachments (the bytes come when opened).
pub async fn store_file_attachments(message_id: &str, remote_id: &str) -> Result<usize> {
    let (s, v) = google::call(Method::GET, &format!("{}/me/messages/{remote_id}/attachments?$top=50&$select=id,name,contentType,size,isInline", graph()), None).await?;
    if !(200..300).contains(&s) {
        bail!("Couldn't read the attachment list ({s}).");
    }
    let list = v["value"].as_array().cloned().unwrap_or_default();
    storage::with_db(|db| {
        let tx = db.unchecked_transaction()?;
        tx.execute("DELETE FROM attachments WHERE message_id=?1 AND is_inline=0", [message_id])?;
        let mut n = 0;
        for a in &list {
            if a["isInline"].as_bool() == Some(true) || a["@odata.type"].as_str().is_some_and(|t| t != "#microsoft.graph.fileAttachment") {
                continue;
            }
            tx.execute(
                "INSERT INTO attachments(id,message_id,remote_attachment_id,filename,mime_type,size_bytes,is_inline,is_downloaded) VALUES(?1,?2,?3,?4,?5,?6,0,0)",
                params![uuid::Uuid::new_v4().to_string(), message_id, a["id"].as_str(), a["name"].as_str().unwrap_or("attachment"), a["contentType"].as_str().unwrap_or("application/octet-stream"), a["size"].as_i64().unwrap_or(0)],
            )?;
            n += 1;
        }
        tx.commit()?;
        Ok(n)
    })
}

/// The attachments of a message. Older Outlook messages get their list from the server the first time.
pub async fn list(message_id: &str) -> Result<Vec<AttachmentRow>> {
    let found = rows(message_id)?;
    if !found.is_empty() {
        return Ok(found);
    }
    let o = owner_of_message(message_id)?;
    if o.has_attachments && o.provider == "microsoft" {
        if let Some(rid) = &o.remote_id {
            let _ = auth::in_account(Some(o.email.clone()), store_file_attachments(message_id, rid)).await; // offline: just no list yet
            return rows(message_id);
        }
    }
    Ok(found)
}

struct Att {
    name: String,
    size: i64,
    remote: Option<String>,
    blob: Option<String>,
    message_id: String,
}

fn load(att_id: &str) -> Result<Att> {
    storage::with_db(|db| {
        db.query_row("SELECT filename, size_bytes, remote_attachment_id, blob_hash, message_id FROM attachments WHERE id=?1", [att_id], |r| {
            Ok(Att { name: r.get(0)?, size: r.get(1)?, remote: r.get(2)?, blob: r.get(3)?, message_id: r.get(4)? })
        })
        .optional()?
        .ok_or(anyhow!("That attachment is no longer available."))
    })
}

/// The file's bytes: from the local copy, else downloaded now (and kept for offline).
pub async fn bytes(att_id: &str) -> Result<(String, Vec<u8>)> {
    let a = load(att_id)?;
    if let Some(h) = &a.blob {
        if let Ok(b) = sync::read_blob(h, "bin") {
            return Ok((a.name, b));
        }
    }
    let o = owner_of_message(&a.message_id)?;
    let rid = o.remote_id.clone().ok_or(anyhow!("This message hasn't been uploaded yet."))?;
    let data = auth::in_account(Some(o.email.clone()), async {
        if o.provider == "google" {
            google::download_attachment(&rid, &a.name, a.size).await
        } else if o.provider == crate::imap::PROVIDER {
            crate::imap::download_attachment(&a.message_id, &a.name, a.size).await
        } else {
            let aid = a.remote.clone().ok_or(anyhow!("Attachment id missing"))?;
            let (s, v) = google::call(Method::GET, &format!("{}/me/messages/{rid}/attachments/{aid}", graph()), None).await?;
            if !(200..300).contains(&s) {
                bail!("The server couldn't give us this file ({s}).");
            }
            Ok(base64::engine::general_purpose::STANDARD.decode(v["contentBytes"].as_str().unwrap_or_default())?)
        }
    })
    .await
    .map_err(|e| if e.to_string().contains("error sending request") { anyhow!("You're offline, and this file hasn't been downloaded yet.") } else { e })?;
    let hash = sync::write_blob(&data, "bin")?;
    storage::with_db(|db| Ok(db.execute("UPDATE attachments SET blob_hash=?1, is_downloaded=1 WHERE id=?2", params![hash, att_id]).map(|_| ())?))?;
    Ok((a.name, data))
}

/// Extensions of files that can run code (or install, mount or change settings) when opened. Attachments with these,
/// or with no extension at all, are only ever saved, never opened from the app.
const RISKY: &[&str] = &[
    // Windows
    "exe", "com", "scr", "pif", "bat", "cmd", "msi", "msp", "mst", "msix", "msixbundle", "appx", "appxbundle", "appinstaller",
    "cpl", "dll", "ocx", "sys", "drv", "hta", "jar", "js", "jse", "vbs", "vbe", "vb", "wsf", "wsh", "wsc", "ws", "ps1", "ps1xml",
    "ps2", "psc1", "psm1", "psd1", "msc", "msh", "reg", "inf", "scf", "lnk", "url", "website", "application", "gadget",
    "settingcontent-ms", "library-ms", "search-ms", "iso", "img", "vhd", "vhdx", "chm", "hlp", "xll", "xlam", "ppam", "mde",
    "accde", "appref-ms", "diagcab", "msdt", "one", "xbap",
    // Linux and other Unix
    "sh", "bash", "zsh", "csh", "ksh", "fish", "run", "bin", "out", "elf", "appimage", "desktop", "deb", "rpm", "snap",
    "flatpakref", "flatpakrepo", "py", "pyc", "pyw", "pl", "rb", "php", "lua", "tcl", "awk", "command", "so", "ko", "apk",
];

/// File names that could run code when opened; these are saved, never opened straight away. Judged on the name as it
/// would be written (so "evil.exe." and "evil.exe " count as .exe, as Windows would treat them).
pub fn risky(name: &str) -> bool {
    let n = safe_name(name).to_lowercase();
    match n.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => RISKY.contains(&ext),
        _ => true, // no extension: the system would decide by content, which can mean "run it"
    }
}

/// Characters that only change how text is shown (right-to-left overrides, invisible marks): "invoice\u{202E}fdp.exe"
/// would display as "invoiceexe.pdf".
fn invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}' | '\u{061C}' | '\u{180E}')
}

const MAX_NAME_BYTES: usize = 180;

/// A file name that is safe on every platform: no path separators, no "..", no control or invisible characters, none of
/// the characters Windows refuses (`< > : " | ? *`), no leading dots, no trailing dots or spaces, not a Windows
/// device name (CON, NUL, COM1, LPT¹…), and not absurdly long (the extension is kept).
pub(crate) fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !invisible(*c))
        .map(|c| if matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') || c.is_control() { '_' } else { c })
        .collect();
    let mut cleaned = cleaned;
    while cleaned.contains("..") {
        cleaned = cleaned.replace("..", ".");
    }
    let mut cleaned = cleaned.trim().trim_start_matches('.').trim_end_matches(['.', ' ']).to_string();
    if cleaned.len() > MAX_NAME_BYTES {
        let ext = cleaned.rsplit_once('.').map(|(_, e)| e).filter(|e| e.len() <= 16).map(|e| format!(".{e}")).unwrap_or_default();
        let mut stem: String = String::new();
        for c in cleaned.chars() {
            if stem.len() + c.len_utf8() + ext.len() > MAX_NAME_BYTES {
                break;
            }
            stem.push(c);
        }
        cleaned = format!("{}{ext}", stem.trim_end_matches(['.', ' ']));
    }
    if cleaned.trim_matches('_').is_empty() || cleaned.is_empty() {
        return "attachment".into();
    }
    // Windows ignores everything from the first dot, and blanks before it, when it looks for device names.
    let stem = cleaned.split('.').next().unwrap_or("").trim_end().to_uppercase();
    let numbered = |p: &str| stem.strip_prefix(p).is_some_and(|d| matches!(d, "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"));
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$") || numbered("COM") || numbered("LPT");
    if device { format!("_{cleaned}") } else { cleaned }
}

/// The path an attachment called `name` gets inside `folder`: a safe name, and never anywhere but directly in the folder.
pub(crate) fn place(folder: &Path, name: &str) -> Result<PathBuf> {
    let folder = dunce::canonicalize(folder).map_err(|_| anyhow!("That folder doesn't exist."))?;
    if !folder.is_dir() {
        bail!("That isn't a folder.");
    }
    let path = folder.join(safe_name(name));
    if path.parent() != Some(folder.as_path()) {
        bail!("That file name can't be used.");
    }
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink() || m.is_dir()) {
        bail!("There is a link or folder with that name already; choose another name.");
    }
    Ok(path)
}

/// Writes an attachment's bytes: a new private file (never through a link), never executable, and on Windows marked as
/// coming from the internet so the system warns before running anything from it.
fn write_file(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = o.open(path)?;
    f.write_all(data)?;
    f.sync_all()?;
    #[cfg(windows)]
    {
        let mut ads = path.as_os_str().to_owned();
        ads.push(":Zone.Identifier");
        let _ = std::fs::write(ads, b"[ZoneTransfer]\r\nZoneId=3\r\n");
    }
    Ok(())
}

/// Where opened attachments are put (a private per-user folder); emptied at every start.
pub fn temp_root() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("graphite-mail").join("opened-attachments")
}

pub fn clear_temp() {
    let _ = std::fs::remove_dir_all(temp_root());
    // Versions before 1.12 used a shared folder in /tmp.
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("graphite-mail-attachments"));
}

/// Writes the file to a private temp folder and returns its path (the caller opens it). Files that could run are
/// refused here, so the app never hands them to the system to open.
pub async fn prepare_open(att_id: &str) -> Result<PathBuf> {
    let a = load(att_id)?;
    if risky(&a.name) {
        bail!("For your safety “{}” can't be opened straight from an email. Use Save, and open it yourself if you trust it.", safe_name(&a.name));
    }
    let (name, data) = bytes(att_id).await?;
    let dir = temp_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for d in [temp_root().parent().unwrap_or(&dir), temp_root().as_path(), dir.as_path()] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let path = place(&dir, &name)?;
    write_file(&path, &data)?;
    Ok(path)
}

/// Saves a copy where the user chose. Only the chosen folder is used as given: the file name is made safe, so the
/// file always lands directly in that folder. Returns where it was written.
pub async fn save(att_id: &str, dest: &str) -> Result<PathBuf> {
    let dest = Path::new(dest);
    if dest.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        bail!("Choose a folder to save into.");
    }
    let folder = dest.parent().filter(|p| !p.as_os_str().is_empty()).ok_or(anyhow!("Choose a folder to save into."))?;
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let path = place(folder, &name)?;
    let (_, data) = bytes(att_id).await?;
    write_file(&path, &data).map_err(|e| anyhow!("Couldn't save the file: {e}"))?;
    Ok(path)
}

/// Files chosen in the editor → blobs + the list stored in the queued message. Enforces the size limit.
pub fn prepare_outgoing(paths: &[String]) -> Result<Vec<Value>> {
    let mut total = 0u64;
    let mut out = vec![];
    for p in paths {
        let path = Path::new(p);
        let meta = std::fs::metadata(path).map_err(|_| anyhow!("Couldn't read {p}."))?;
        if !meta.is_file() {
            bail!("{p} is not a file.");
        }
        total += meta.len();
        if total > MAX_OUTGOING {
            bail!("Attachments are limited to {} MB in total.", MAX_OUTGOING >> 20);
        }
        let bytes = std::fs::read(path)?;
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "attachment".into());
        let mime = mime_guess::from_path(path).first_or_octet_stream().to_string();
        out.push(json!({ "name": name, "mime": mime, "size": bytes.len(), "blob": sync::write_blob(&bytes, "bin")? }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google::tests::fake_google;

    fn pdf_b64() -> String {
        base64::engine::general_purpose::STANDARD.encode(b"%PDF-1.4 hello")
    }

    fn setup() -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!("gm-att-{}", uuid::Uuid::new_v4()));
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.join("cfg"));
            std::env::set_var("GM_TEST_TOKEN", "t");
        }
        storage::initialize(tmp.join("vault").to_str().unwrap()).unwrap();
        storage::with_db(|db| {
            db.execute_batch(
                "INSERT INTO accounts(id,email,display_name,created_at,provider) VALUES('m','me@x.com','Me',1,'microsoft'),('g','me@gmail.com','Gm',2,'google');
                 INSERT INTO folders(id,account_id,remote_id,display_name,well_known_name) VALUES('mi','m','MI','Inbox','inbox'),('gi','g','INBOX','Inbox','inbox');
                 INSERT INTO messages(id,account_id,folder_id,remote_id,subject,received_at,has_attachments,body_cached) VALUES
                   ('m1','m','mi','r1','With file',1,1,1),('g1','g','gi','gm1','Gmail file',2,1,1);",
            )?;
            Ok(())
        })
        .unwrap();
        tmp
    }

    #[test]
    fn dangerous_files_are_not_opened() {
        for n in ["setup.EXE", "run.sh", "x.AppImage", "evil.desktop", "a.ps1"] {
            assert!(risky(n), "{n}");
        }
        for n in ["report.pdf", "photo.jpg", "notes.txt", "sheet.xlsx"] {
            assert!(!risky(n), "{n}");
        }
    }

    /// Names an attacker could put in an email, and what they become.
    const HOSTILE: &[(&str, &str)] = &[
        ("../../etc/passwd", "_._etc_passwd"),
        ("..\\..\\Windows\\System32\\evil.dll", "_._Windows_System32_evil.dll"),
        ("/etc/cron.d/backdoor", "_etc_cron.d_backdoor"),
        ("C:\\Windows\\win.ini", "C__Windows_win.ini"),
        ("\\\\server\\share\\x.txt", "__server_share_x.txt"),
        ("a/../../b.txt", "a_._._b.txt"),
        ("~/.ssh/authorized_keys", "~_.ssh_authorized_keys"),
        ("..", "attachment"),
        (".", "attachment"),
        ("...", "attachment"),
        ("", "attachment"),
        ("   ", "attachment"),
        ("/", "attachment"),
        ("..\\", "attachment"),
        (".bashrc", "bashrc"),
        ("CON", "_CON"),
        ("con.txt", "_con.txt"),
        ("NUL.tar.gz", "_NUL.tar.gz"),
        ("COM1", "_COM1"),
        ("lpt9.doc", "_lpt9.doc"),
        ("CON .txt", "_CON .txt"),
        ("AUX.", "_AUX"),
        ("CONIN$", "_CONIN$"),
        ("COM¹.txt", "_COM¹.txt"),
        ("COM10.txt", "COM10.txt"),
        ("CONSOLE.txt", "CONSOLE.txt"),
        ("file\u{0}name.txt", "file_name.txt"),
        ("line\nbreak\r.txt", "line_break_.txt"),
        ("tab\tname.pdf", "tab_name.pdf"),
        ("bell\u{7}\u{1b}[31m.txt", "bell__[31m.txt"),
        ("invoice\u{202E}fdp.exe", "invoicefdp.exe"),
        ("\u{202E}gpj.sh", "gpj.sh"),
        ("zero\u{200B}width\u{FEFF}.pdf", "zerowidth.pdf"),
        ("report:Zone.Identifier", "report_Zone.Identifier"),
        ("what?<>|*\".txt", "what______.txt"),
        ("evil.exe.", "evil.exe"),
        ("evil.exe   ", "evil.exe"),
        ("photo.jpg.exe", "photo.jpg.exe"),
        ("Ünïcødé – ok.pdf", "Ünïcødé – ok.pdf"),
    ];

    #[test]
    fn hostile_attachment_names_are_made_safe() {
        for (raw, want) in HOSTILE {
            let got = safe_name(raw);
            assert_eq!(&got, want, "{raw:?}");
            assert!(!got.contains(['/', '\\', ':', '\0']) && !got.contains("..") && !got.chars().any(char::is_control), "{raw:?} -> {got:?}");
            assert!(!got.starts_with('.') && !got.ends_with(['.', ' ']), "{got:?}");
        }
        // very long names are cut, keeping the extension
        let long = safe_name(&format!("{}.pdf", "a".repeat(400)));
        assert!(long.len() <= 180 && long.ends_with(".pdf"), "{long}");
        let multi = safe_name(&format!("{}.pdf", "é".repeat(300)));
        assert!(multi.len() <= 180 && multi.ends_with(".pdf"));
        // a name that is only forbidden characters
        assert_eq!(safe_name("???"), "attachment");
    }

    #[test]
    fn programs_and_scripts_are_never_opened() {
        for n in ["setup.EXE", "run.sh", "x.AppImage", "evil.desktop", "a.ps1", "a.bat", "a.cmd", "a.msi", "a.hta", "a.lnk", "a.jar", "a.vbs", "a.reg",
            "a.scr", "a.iso", "a.deb", "a.py", "evil.exe.", "evil.exe ", "invoice\u{202E}fdp.exe", "photo.jpg.exe", "noextension", ".bashrc", "Makefile"] {
            assert!(risky(n), "{n}");
        }
        for n in ["report.pdf", "photo.jpg", "notes.txt", "sheet.xlsx", "slides.pptx", "archive.zip", "a.docx", "my.sh.txt"] {
            assert!(!risky(n), "{n}");
        }
    }

    #[test]
    fn saved_and_opened_files_stay_in_their_folder() {
        let dir = std::env::temp_dir().join(format!("gm-place-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let folder = dunce::canonicalize(&dir).unwrap();
        for (raw, _) in HOSTILE {
            let p = place(&dir, raw).unwrap();
            assert_eq!(p.parent(), Some(folder.as_path()), "{raw:?} escaped: {}", p.display());
            write_file(&p, b"x").unwrap();
            assert_eq!(std::fs::read(&p).unwrap(), b"x");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "private and never executable: {raw:?}");
            }
            std::fs::remove_file(&p).unwrap();
        }
        // nothing was written outside the folder
        assert!(std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).all(|e| e.file_name() == "sub"));
        // an existing folder or link with that name is not written through
        assert!(place(&dir, "sub").is_err());
        #[cfg(unix)]
        {
            let outside = std::env::temp_dir().join(format!("gm-outside-{}", uuid::Uuid::new_v4()));
            std::fs::write(&outside, b"keep").unwrap();
            std::os::unix::fs::symlink(&outside, dir.join("link.txt")).unwrap();
            assert!(place(&dir, "link.txt").is_err());
            assert!(write_file(&dir.join("link.txt"), b"evil").is_err(), "O_NOFOLLOW");
            assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
            let _ = std::fs::remove_file(outside);
        }
        assert!(place(&dir.join("missing"), "a.txt").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn attachments_list_download_open_and_save() {
        let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = setup();
        let mut pdf = tmp.clone();
        pdf.push("unused");
        let gmail_raw = crate::google::tests::raw_mail("Gmail file", "<p>x</p>", None, true);
        let (base, log) = fake_google(move |m, p, _| match (m, p.split('?').next().unwrap()) {
            ("GET", "/me/messages/r1/attachments") => (200, json!({ "value": [
                { "@odata.type": "#microsoft.graph.fileAttachment", "id": "A1", "name": "contract.pdf", "contentType": "application/pdf", "size": 14, "isInline": false },
                { "@odata.type": "#microsoft.graph.fileAttachment", "id": "A2", "name": "logo.png", "contentType": "image/png", "size": 3, "isInline": true },
                { "@odata.type": "#microsoft.graph.itemAttachment", "id": "A3", "name": "forwarded mail", "size": 5 } ] })),
            ("GET", "/me/messages/r1/attachments/A1") => (200, json!({ "contentBytes": pdf_b64() })),
            ("GET", "/gmail/v1/users/me/messages/gm1") => (200, json!({ "id": "gm1", "raw": gmail_raw })),
            _ => (404, json!({ "error": { "message": "no route" } })),
        })
        .await;
        unsafe {
            std::env::set_var("GM_TEST_GRAPH", &base);
            std::env::set_var("GM_TEST_GOOGLE", &base);
        }

        // Outlook: names come from the server once, only real files are listed
        let list = list("m1").await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].name.as_str(), list[0].size, list[0].downloaded), ("contract.pdf", 14, false));
        let calls_before = log.lock().unwrap().len();
        assert_eq!(crate::attachments::list("m1").await.unwrap().len(), 1);
        assert_eq!(log.lock().unwrap().len(), calls_before, "the list is kept locally");

        // downloading keeps a local copy
        let (name, data) = bytes(&list[0].id).await.unwrap();
        assert_eq!((name.as_str(), data.as_slice()), ("contract.pdf", b"%PDF-1.4 hello".as_slice()));
        let n = log.lock().unwrap().len();
        bytes(&list[0].id).await.unwrap();
        assert_eq!(log.lock().unwrap().len(), n, "second time it comes from the local copy");
        assert!(list_downloaded("m1"));

        // opening writes a private temp file; saving copies it
        let path = prepare_open(&list[0].id).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"%PDF-1.4 hello");
        assert!(path.starts_with(temp_root()));
        let dest = tmp.join("saved.pdf");
        assert_eq!(save(&list[0].id, dest.to_str().unwrap()).await.unwrap(), dunce::canonicalize(&tmp).unwrap().join("saved.pdf"));
        assert_eq!(std::fs::read(dest).unwrap(), b"%PDF-1.4 hello");
        // a hostile name in the save path still lands in the chosen folder
        let written = save(&list[0].id, tmp.join("evil\u{202E}fdp.sh:x").to_str().unwrap()).await.unwrap();
        assert_eq!(written, dunce::canonicalize(&tmp).unwrap().join("evilfdp.sh_x"));
        for bad in [tmp.join("..").join("up.pdf"), tmp.join("sub").join("..").join("..").join("up.pdf")] {
            assert!(save(&list[0].id, bad.to_str().unwrap()).await.is_err(), "{}", bad.display());
        }
        assert!(!tmp.parent().unwrap().join("up.pdf").exists());
        assert!(save(&list[0].id, "").await.is_err());
        assert!(save(&list[0].id, "relative.pdf").await.is_err(), "a folder must be chosen");
        clear_temp();
        assert!(!path.exists());

        // risky types refuse to open
        storage::with_db(|db| {
            db.execute("INSERT INTO attachments(id,message_id,remote_attachment_id,filename,mime_type,size_bytes,is_inline,is_downloaded) VALUES('bad','m1','A9','install.sh','text/x-sh',3,0,0)", [])?;
            Ok(())
        })
        .unwrap();
        assert!(prepare_open("bad").await.unwrap_err().to_string().contains("For your safety"));

        // Gmail: attachment metadata was stored at sync; bytes are found again in the full message
        storage::with_db(|db| {
            db.execute("INSERT INTO attachments(id,message_id,filename,mime_type,size_bytes,is_inline,is_downloaded) VALUES('gatt','g1','bill.pdf','application/pdf',13,0,0)", [])?;
            Ok(())
        })
        .unwrap();
        let (_, gdata) = bytes("gatt").await.unwrap();
        assert_eq!(gdata, b"%PDF-1.4 fake");
    }

    fn list_downloaded(message_id: &str) -> bool {
        rows(message_id).unwrap().iter().all(|r| r.downloaded)
    }

    #[test]
    fn outgoing_files_are_checked_and_stored() {
        let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = setup();
        let f = tmp.join("report.pdf");
        std::fs::write(&f, b"data").unwrap();
        let out = prepare_outgoing(&[f.to_string_lossy().to_string()]).unwrap();
        assert_eq!((out[0]["name"].as_str(), out[0]["mime"].as_str(), out[0]["size"].as_i64()), (Some("report.pdf"), Some("application/pdf"), Some(4)));
        assert_eq!(sync::read_blob(out[0]["blob"].as_str().unwrap(), "bin").unwrap(), b"data");
        let big = tmp.join("big.bin");
        std::fs::write(&big, vec![0u8; (MAX_OUTGOING + 1) as usize]).unwrap();
        assert!(prepare_outgoing(&[big.to_string_lossy().to_string()]).unwrap_err().to_string().contains("limited to 15 MB"));
        assert!(prepare_outgoing(&["/no/such/file".into()]).is_err());
        assert!(prepare_outgoing(&[tmp.to_string_lossy().to_string()]).unwrap_err().to_string().contains("not a file"));
    }

    #[tokio::test]
    async fn sending_with_attachments_on_both_providers() {
        use crate::outbox::{Draft, pending_count, replay, send};
        let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = setup();
        let small = tmp.join("small.txt");
        let large = tmp.join("large.bin");
        std::fs::write(&small, b"small file").unwrap();
        std::fs::write(&large, vec![7u8; 3_500_000]).unwrap();
        let (paths, ) = (vec![small.to_string_lossy().to_string(), large.to_string_lossy().to_string()],);
        let gmail_orig = crate::google::tests::raw_mail("Original", "<p>orig</p>", None, true);
        let (base, log) = fake_google(move |m, p, _| match (m, p.split('?').next().unwrap()) {
            ("POST", "/me/messages") => (201, json!({ "id": "D1" })),
            ("POST", "/me/messages/r1/createReply") => (201, json!({ "id": "D2" })),
            ("PATCH", "/me/messages/D2") => (200, json!({})),
            ("POST", x) if x.ends_with("/attachments") => (201, json!({})),
            ("POST", x) if x.ends_with("/createUploadSession") => (201, json!({ "uploadUrl": format!("{}/upload/1", std::env::var("GM_TEST_GRAPH").unwrap()) })),
            ("PUT", "/upload/1") => (200, json!({})),
            ("POST", x) if x.ends_with("/send") => (202, Value::Null),
            ("GET", "/gmail/v1/users/me/messages/gm1") => (200, json!({ "id": "gm1", "raw": gmail_orig })),
            ("POST", "/gmail/v1/users/me/messages/send") => (200, json!({ "id": "s" })),
            _ => (404, json!({ "error": { "message": "no route" } })),
        })
        .await;
        unsafe {
            std::env::set_var("GM_TEST_GRAPH", &base);
            std::env::set_var("GM_TEST_GOOGLE", &base);
        }

        // Outlook, new message: draft -> small file -> big file by upload session in 2 pieces -> send
        send(Draft { mode: "new".into(), reply_to: None, to: vec!["a@b.c".into()], cc: vec![], bcc: vec!["hidden@b.c".into()], subject: "Files".into(), html: "<p>hi</p>".into(), from: Some("me@x.com".into()), attachments: paths.clone() }).unwrap();
        assert_eq!(replay().await.unwrap(), 1);
        let seq: Vec<String> = log.lock().unwrap().iter().map(|(m, p, _)| format!("{m} {}", p.split('?').next().unwrap())).collect();
        assert_eq!(seq, vec!["POST /me/messages", "POST /me/messages/D1/attachments", "POST /me/messages/D1/attachments/createUploadSession", "PUT /upload/1", "PUT /upload/1", "POST /me/messages/D1/send"]);
        let first = log.lock().unwrap()[0].2.clone();
        assert_eq!(first["subject"], "Files");
        // The draft-with-files path keeps blind copies out of To/Cc too.
        assert_eq!(first["bccRecipients"][0]["emailAddress"]["address"], "hidden@b.c");
        assert_eq!((first["toRecipients"].as_array().unwrap().len(), first["ccRecipients"].as_array().unwrap().len()), (1, 0));
        assert!(!first["toRecipients"].to_string().contains("hidden"));
        assert_eq!(log.lock().unwrap()[1].2["name"], "small.txt");
        assert_eq!(pending_count().unwrap(), 0);

        // Outlook reply: a reply draft is made, filled in, then sent with the file
        log.lock().unwrap().clear();
        send(Draft { mode: "reply".into(), reply_to: Some("m1".into()), to: vec!["a@b.c".into()], cc: vec![], bcc: vec![], subject: "Re".into(), html: "<p>r</p>".into(), from: None, attachments: vec![small.to_string_lossy().to_string()] }).unwrap();
        replay().await.unwrap();
        let seq: Vec<String> = log.lock().unwrap().iter().map(|(m, p, _)| format!("{m} {}", p.split('?').next().unwrap())).collect();
        assert_eq!(seq, vec!["POST /me/messages/r1/createReply", "PATCH /me/messages/D2", "POST /me/messages/D2/attachments", "POST /me/messages/D2/send"]);

        // Gmail: the file is inside the message; a forward also carries the original's file
        log.lock().unwrap().clear();
        send(Draft { mode: "forward".into(), reply_to: Some("g1".into()), to: vec!["z@z.com".into()], cc: vec![], bcc: vec![], subject: "Fw: Original".into(), html: "<p>fyi</p>".into(), from: None, attachments: vec![small.to_string_lossy().to_string()] }).unwrap();
        replay().await.unwrap();
        let sent = log.lock().unwrap().iter().find(|(_, p, _)| p.ends_with("/messages/send")).cloned().unwrap();
        let raw = crate::google::tests::unb64_for_test(sent.2["raw"].as_str().unwrap());
        let parsed = crate::google::parse_raw_full(&raw, true).unwrap();
        let names: Vec<&str> = parsed.files.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(names, vec!["small.txt", "bill.pdf"]);
        assert_eq!(parsed.file_bytes[0], b"small file");
        assert_eq!(pending_count().unwrap(), 0);
    }
}
