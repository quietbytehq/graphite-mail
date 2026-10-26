//! Microsoft Graph mail sync: folder hierarchy, per-folder message delta, and offline body hydration.

use crate::{auth, storage};
use anyhow::{Result, anyhow};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;

const GRAPH: &str = "https://graph.microsoft.com/v1.0";
const MSG_FIELDS: &str = "id,conversationId,internetMessageId,subject,from,toRecipients,ccRecipients,receivedDateTime,isRead,flag,isDraft,hasAttachments,bodyPreview,changeKey";

async fn get(url: &str) -> Result<Value> {
    let token = auth::access_token().await?;
    let resp = crate::http::client()
        .get(url)
        .bearer_auth(token)
        .header(
            "Prefer",
            "odata.maxpagesize=100, outlook.body-content-type=\"html\"",
        )
        .send()
        .await?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(anyhow!(
            "Graph {status}: {}",
            body["error"]["message"].as_str().unwrap_or("")
        ));
    }
    Ok(body)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn ensure_account(acc: &auth::Account) -> Result<String> {
    storage::with_db(|db| {
        if let Some(id) = db
            .query_row(
                "SELECT id FROM accounts WHERE email=?1",
                [&acc.email],
                |r| r.get(0),
            )
            .optional()?
        {
            db.execute("UPDATE accounts SET provider=?1, display_name=?2 WHERE id=?3", params![acc.provider, acc.display_name, id])?;
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO accounts(id,email,display_name,created_at,provider) VALUES(?1,?2,?3,?4,?5)",
            params![id, acc.email, acc.display_name, now(), acc.provider],
        )?;
        Ok(id)
    })
}

#[derive(Serialize, Default, Clone)]
pub struct SyncReport {
    pub folders: usize,
    pub messages_changed: usize,
    pub bodies_downloaded: usize,
    pub replayed: usize,
    pub events: usize,
    /// Set when sending your offline changes failed, or the calendar couldn't be updated.
    pub replay_error: Option<String>,
    pub calendar_error: Option<String>,
}

/// Only one sync at a time; a data-folder move also takes this so nothing is saved mid-move.
pub static SYNC_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Deletes everything downloaded for an account (its folders, mail, calendars cascade from the account row).
pub fn forget_account_data(email: &str) -> Result<()> {
    storage::with_db(|db| {
        db.execute(
            "DELETE FROM messages_fts WHERE message_id IN (SELECT m.id FROM messages m JOIN accounts a ON a.id=m.account_id WHERE lower(a.email)=lower(?1))",
            [email],
        )?;
        db.execute("DELETE FROM accounts WHERE lower(email)=lower(?1)", [email])?;
        Ok(())
    })
}

pub fn ensure_account_pub(acc: &auth::Account) -> Result<String> {
    ensure_account(acc)
}

/// Full sync pass for every signed-in account: replay offline changes, then folders → messages → bodies → calendar.
pub async fn sync_all() -> Result<SyncReport> {
    let _only_one = SYNC_LOCK.lock().await;
    let accounts = auth::accounts();
    if accounts.is_empty() {
        return Err(anyhow!("Not signed in"));
    }
    let mut rep = SyncReport::default();
    // Replay offline actions before pulling, so server state never overwrites local changes.
    match crate::outbox::replay().await {
        Ok(n) => rep.replayed = n,
        Err(e) => rep.replay_error = Some(e.to_string()),
    }
    let mut errors = vec![];
    for acc in &accounts {
        let account_id = ensure_account(acc)?;
        let r = match acc.provider.as_str() {
            "google" => auth::in_account(Some(acc.email.clone()), crate::google::sync_account(&account_id)).await,
            crate::imap::PROVIDER => auth::in_account(Some(acc.email.clone()), crate::imap::sync_account(&account_id)).await,
            _ => auth::in_account(Some(acc.email.clone()), sync_microsoft(&account_id)).await,
        };
        match r {
            Ok(part) => {
                // Gmail (app password): "People you've emailed" grows with the mail just synced; never holds up the sync.
                if acc.provider == crate::imap::PROVIDER {
                    if let Err(e) = crate::local_contacts::rebuild_people(&account_id) {
                        eprintln!("[graphite] people list of {} not updated: {e}", acc.email);
                    }
                }
                rep.folders += part.folders;
                rep.messages_changed += part.messages_changed;
                rep.bodies_downloaded += part.bodies_downloaded;
                rep.events += part.events;
                if part.calendar_error.is_some() {
                    rep.calendar_error = part.calendar_error;
                }
            }
            Err(e) => {
                eprintln!("[graphite] sync of {} failed: {e}", acc.email);
                errors.push(format!("{}: {e}", acc.email));
            }
        }
    }
    if errors.len() == accounts.len() {
        return Err(anyhow!(errors.join("; ")));
    }
    if !errors.is_empty() {
        rep.calendar_error = Some(errors.join("; "));
    }
    Ok(rep)
}

/// One Microsoft 365 account (runs inside auth::in_account for that account).
async fn sync_microsoft(account_id: &str) -> Result<SyncReport> {
    let mut rep = SyncReport::default();
    rep.folders = sync_folders(account_id).await?;
    let folders: Vec<(String, String, Option<String>)> = storage::with_db(|db| {
        let mut st =
            db.prepare("SELECT id, remote_id, delta_link FROM folders WHERE account_id=?1")?;
        let rows = st.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })?;
    for (fid, remote, delta) in folders {
        rep.messages_changed += sync_folder_messages(account_id, &fid, &remote, delta).await?;
    }
    rep.bodies_downloaded = hydrate_bodies(account_id, 200).await?;
    let _ = backfill_inline(account_id, 100).await;
    match crate::calendar::sync(account_id).await {
        Ok(n) => rep.events = n,
        Err(e) => {
            eprintln!("[graphite] calendar sync failed: {e}");
            rep.calendar_error = Some(e.to_string());
        }
    }
    // Contacts come last and never hold up mail or calendar: how they went is shown in the Contacts view.
    let _ = crate::contacts::sync_account(account_id, false).await;
    Ok(rep)
}

async fn sync_folders(account_id: &str) -> Result<usize> {
    let mut stack = vec![format!(
        "{GRAPH}/me/mailFolders?$top=100&includeHiddenFolders=false"
    )];
    let well_known = [
        "inbox",
        "drafts",
        "sentitems",
        "deleteditems",
        "junkemail",
        "archive",
        "outbox",
        // Roles the sidebar gives their own tile; mailboxes without them answer 404, which is ignored below.
        "conversationhistory",
        "syncissues",
    ];
    let mut wk_ids = std::collections::HashMap::new();
    for w in well_known {
        if let Ok(v) = get(&format!("{GRAPH}/me/mailFolders/{w}?$select=id")).await {
            if let Some(id) = v["id"].as_str() {
                wk_ids.insert(id.to_string(), w);
            }
        }
    }
    let mut count = 0;
    while let Some(url) = stack.pop() {
        let page = get(&url).await?;
        for f in page["value"].as_array().cloned().unwrap_or_default() {
            let rid = f["id"].as_str().unwrap_or_default().to_string();
            storage::with_db(|db| {
                db.execute(
                    "INSERT INTO folders(id,account_id,remote_id,parent_remote_id,display_name,well_known_name,unread_count,total_count)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                     ON CONFLICT(account_id,remote_id) DO UPDATE SET display_name=excluded.display_name,
                       parent_remote_id=excluded.parent_remote_id, unread_count=excluded.unread_count, total_count=excluded.total_count",
                    params![
                        uuid::Uuid::new_v4().to_string(), account_id, rid,
                        f["parentFolderId"].as_str(), f["displayName"].as_str().unwrap_or("(folder)"),
                        wk_ids.get(&rid).copied(), f["unreadItemCount"].as_i64().unwrap_or(0), f["totalItemCount"].as_i64().unwrap_or(0)
                    ],
                )?;
                Ok(())
            })?;
            count += 1;
            if f["childFolderCount"].as_i64().unwrap_or(0) > 0 {
                stack.push(format!(
                    "{GRAPH}/me/mailFolders/{rid}/childFolders?$top=100"
                ));
            }
        }
        if let Some(next) = page["@odata.nextLink"].as_str() {
            stack.push(next.to_string());
        }
    }
    Ok(count)
}

fn recipients_json(v: &Value) -> String {
    let list: Vec<Value> = v
        .as_array()
        .map(|a| a.iter().map(|r| r["emailAddress"].clone()).collect())
        .unwrap_or_default();
    Value::Array(list).to_string()
}

async fn sync_folder_messages(
    account_id: &str,
    folder_id: &str,
    remote: &str,
    delta: Option<String>,
) -> Result<usize> {
    let mut url = delta.unwrap_or_else(|| {
        format!("{GRAPH}/me/mailFolders/{remote}/messages/delta?$select={MSG_FIELDS}")
    });
    let mut changed = 0;
    loop {
        let page = match get(&url).await {
            Ok(p) => p,
            Err(e) if e.to_string().contains("410") => {
                // Delta token expired: clear it so the next pass does a fresh full sync.
                storage::with_db(|db| {
                    Ok(db.execute(
                        "UPDATE folders SET delta_link=NULL WHERE id=?1",
                        [folder_id],
                    )?)
                })?;
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let items = page["value"].as_array().cloned().unwrap_or_default();
        storage::with_db(|db| {
            let tx = db.unchecked_transaction()?;
            for m in &items {
                let rid = m["id"].as_str().unwrap_or_default();
                if m.get("@removed").is_some() {
                    tx.execute("DELETE FROM messages_fts WHERE message_id IN (SELECT id FROM messages WHERE remote_id=?1)", [rid])?;
                    tx.execute("DELETE FROM messages WHERE remote_id=?1", [rid])?;
                    continue;
                }
                let received = m["receivedDateTime"]
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.timestamp())
                    .unwrap_or(0);
                tx.execute(
                    "INSERT INTO messages(id,account_id,folder_id,remote_id,internet_message_id,conversation_id,subject,sender_name,sender_address,
                        to_recipients_json,cc_recipients_json,snippet,received_at,is_read,is_flagged,is_draft,has_attachments,change_key)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
                     ON CONFLICT(remote_id) DO UPDATE SET folder_id=excluded.folder_id, subject=excluded.subject,
                        is_read=excluded.is_read, is_flagged=excluded.is_flagged, snippet=excluded.snippet,
                        change_key=excluded.change_key,
                        body_cached=CASE WHEN messages.change_key=excluded.change_key THEN messages.body_cached ELSE 0 END
                     WHERE messages.sync_status='SYNCED'",
                    params![
                        uuid::Uuid::new_v4().to_string(), account_id, folder_id, rid,
                        m["internetMessageId"].as_str(), m["conversationId"].as_str(), m["subject"].as_str(),
                        m["from"]["emailAddress"]["name"].as_str(), m["from"]["emailAddress"]["address"].as_str(),
                        recipients_json(&m["toRecipients"]), recipients_json(&m["ccRecipients"]),
                        m["bodyPreview"].as_str(), received,
                        m["isRead"].as_bool().unwrap_or(false), m["flag"]["flagStatus"].as_str() == Some("flagged"),
                        m["isDraft"].as_bool().unwrap_or(false), m["hasAttachments"].as_bool().unwrap_or(false),
                        m["changeKey"].as_str()
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })?;
        changed += items.len();
        if let Some(next) = page["@odata.nextLink"].as_str() {
            url = next.to_string();
        } else {
            if let Some(dl) = page["@odata.deltaLink"].as_str() {
                storage::with_db(|db| {
                    Ok(db.execute(
                        "UPDATE folders SET delta_link=?1 WHERE id=?2",
                        params![dl, folder_id],
                    )?)
                })?;
            }
            break;
        }
    }
    Ok(changed)
}

pub(crate) fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Content-addressable store: blobs/ab/abcdef….html
pub fn write_blob(bytes: &[u8], ext: &str) -> Result<String> {
    let hash: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = storage::blob_dir()?.join(&hash[..2]);
    storage::private_dir(&dir)?;
    let path = dir.join(format!("{hash}.{ext}"));
    if !path.exists() {
        let tmp = dir.join(format!("{hash}.tmp"));
        std::io::Write::write_all(&mut storage::private_file(&tmp)?, bytes)?;
        fs::rename(tmp, &path)?;
    }
    Ok(hash)
}

pub fn read_blob(hash: &str, ext: &str) -> Result<Vec<u8>> {
    Ok(fs::read(
        storage::blob_dir()?
            .join(&hash[..2])
            .join(format!("{hash}.{ext}")),
    )?)
}

/// Downloads full HTML bodies for newest messages first so mail is readable offline.
pub async fn hydrate_bodies(account_id: &str, limit: usize) -> Result<usize> {
    let pending: Vec<(String, String, String, String, String, bool)> = storage::with_db(|db| {
        let mut st = db.prepare(
            "SELECT id, remote_id, COALESCE(subject,''), COALESCE(sender_name,''), COALESCE(sender_address,''), has_attachments
             FROM messages WHERE account_id=?2 AND body_cached=0 AND remote_id IS NOT NULL ORDER BY received_at DESC LIMIT ?1",
        )?;
        let rows = st.query_map(params![limit as i64, account_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    })?;
    let mut n = 0;
    for (id, rid, subject, sname, saddr, has_att) in pending {
        let v = match get(&format!("{GRAPH}/me/messages/{rid}?$select=body")).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let html = v["body"]["content"].as_str().unwrap_or_default();
        let hash = write_blob(html.as_bytes(), "html")?;
        let text = strip_html(html);
        // Inline pictures arrive as attachments; fetch them now so they show offline. If this fails,
        // leave inline_done=0 so the next pass tries again.
        let inline_done = !has_cid(html) || fetch_inline_images(&id, &rid).await.is_ok();
        if has_att {
            let _ = crate::attachments::store_file_attachments(&id, &rid).await; // names/sizes; the files come when opened
        }
        storage::with_db(|db| {
            db.execute(
                "UPDATE messages SET body_blob_hash=?1, body_cached=1, inline_done=?3 WHERE id=?2",
                params![hash, id, inline_done],
            )?;
            db.execute("DELETE FROM messages_fts WHERE message_id=?1", [&id])?;
            db.execute(
                "INSERT INTO messages_fts(message_id,subject,sender_name,sender_address,body_plaintext) VALUES(?1,?2,?3,?4,?5)",
                params![id, subject, sname, saddr, text],
            )?;
            Ok(())
        })?;
        n += 1;
    }
    Ok(n)
}

/// True when the HTML refers to embedded (cid:) pictures.
pub fn has_cid(html: &str) -> bool {
    html.to_ascii_lowercase().contains("cid:")
}

/// Downloads the inline (cid:) pictures of one message and stores them in the blob store.
async fn fetch_inline_images(message_id: &str, remote_id: &str) -> Result<usize> {
    let list = get(&format!(
        "{GRAPH}/me/messages/{remote_id}/attachments?$top=50"
    ))
    .await?;
    let mut n = 0;
    storage::with_db(|db| {
        Ok(db.execute(
            "DELETE FROM attachments WHERE message_id=?1 AND is_inline=1",
            [message_id],
        )?)
    })?;
    for a in list["value"].as_array().cloned().unwrap_or_default() {
        if a["@odata.type"].as_str() != Some("#microsoft.graph.fileAttachment") {
            continue;
        }
        let Some(cid) = a["contentId"].as_str().filter(|c| !c.is_empty()) else {
            continue;
        };
        let aid = a["id"].as_str().unwrap_or_default();
        // The list can leave out the bytes of larger files; ask for the single attachment then.
        let bytes_b64 = match a["contentBytes"].as_str() {
            Some(b) => b.to_string(),
            None => get(&format!(
                "{GRAPH}/me/messages/{remote_id}/attachments/{aid}"
            ))
            .await?["contentBytes"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        };
        let bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            bytes_b64.as_bytes(),
        )?;
        let blob = write_blob(&bytes, "bin")?;
        storage::with_db(|db| {
            db.execute(
                "INSERT INTO attachments(id,message_id,remote_attachment_id,filename,mime_type,size_bytes,content_id,is_inline,blob_hash,is_downloaded)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8,1)",
                params![
                    uuid::Uuid::new_v4().to_string(), message_id, aid,
                    a["name"].as_str().unwrap_or("image"), a["contentType"].as_str().unwrap_or("application/octet-stream"),
                    bytes.len() as i64, cid, blob
                ],
            )?;
            Ok(())
        })?;
        n += 1;
    }
    Ok(n)
}

/// Messages saved before inline pictures were supported: check their saved body, fetch pictures where needed.
async fn backfill_inline(account_id: &str, limit: usize) -> Result<usize> {
    let todo: Vec<(String, String, String)> = storage::with_db(|db| {
        let mut st = db.prepare(
            "SELECT id, remote_id, body_blob_hash FROM messages
             WHERE account_id=?2 AND body_cached=1 AND inline_done=0 AND remote_id IS NOT NULL AND body_blob_hash IS NOT NULL
             ORDER BY received_at DESC LIMIT ?1",
        )?;
        let rows = st.query_map(params![limit as i64, account_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    })?;
    let mut fetched = 0;
    for (id, rid, hash) in todo {
        let html = read_blob(&hash, "html")
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let ok = if has_cid(&html) {
            fetched += 1;
            fetch_inline_images(&id, &rid).await.is_ok()
        } else {
            true
        };
        if ok {
            storage::with_db(|db| {
                Ok(db.execute("UPDATE messages SET inline_done=1 WHERE id=?1", [&id])?)
            })?;
        }
        if fetched >= 25 {
            break;
        }
    }
    Ok(fetched)
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_cid_references() {
        assert!(super::has_cid("<img src=\"CID:abc\">"));
        assert!(!super::has_cid("<img src=\"https://x/y.png\">"));
    }
    #[test]
    fn strips_html() {
        assert_eq!(
            super::strip_html("<p>Hello <b>there</b></p>\n<br>world"),
            "Hello there world"
        );
    }
}
