//! Gmail + Google Calendar provider: OAuth (desktop client, PKCE, loopback), label/message sync through the
//! Gmail API (history API for incremental changes), offline actions, and Google Calendar.
//!
//! Mapping onto the shared tables: a Gmail label is a folder; a message's *primary* label is `messages.folder_id`
//! and every label it carries is also recorded in `message_labels`, so it shows under each of them.

use crate::{auth, storage, sync};
use anyhow::{Result, anyhow, bail};
use base64::Engine;
use futures::stream::{self, StreamExt};
use mail_parser::{MessageParser, MimeHeaders};
use reqwest::Method;
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
const SCOPES: &str = "openid email profile https://www.googleapis.com/auth/gmail.modify https://www.googleapis.com/auth/calendar";

/// OAuth client of type "Desktop app". Both values are fixed at build time (build.rs) from the environment or the
/// untracked file src-tauri/google_oauth.json; the secret is never in the source. They can still be overridden at
/// run time with GRAPHITE_GOOGLE_CLIENT_ID / GRAPHITE_GOOGLE_CLIENT_SECRET (handy for development).
const CLIENT_ID: &str = match option_env!("GRAPHITE_GOOGLE_CLIENT_ID") {
    Some(id) => id,
    None => "860043772140-ko3lhq0vup2e4v3h5lirjj4uh7t3nlgm.apps.googleusercontent.com",
};
const CLIENT_SECRET: &str = match option_env!("GRAPHITE_GOOGLE_CLIENT_SECRET") {
    Some(s) => s,
    None => "",
};

const FILTERS_SCOPE: &str = "https://www.googleapis.com/auth/gmail.settings.basic";

/// First sync: how far back, and at most how many messages, to download.
const INITIAL_DAYS: i64 = 90;
const INITIAL_MAX: usize = 3000;
const PAST_DAYS: i64 = 90;
const FUTURE_DAYS: i64 = 365;
const PARALLEL: usize = 6;

// ---------------------------------------------------------------- endpoints (overridable in tests)

pub(crate) fn gmail() -> String {
    #[cfg(test)]
    if let Ok(b) = std::env::var("GM_TEST_GOOGLE") {
        return format!("{b}/gmail/v1/users/me");
    }
    "https://gmail.googleapis.com/gmail/v1/users/me".into()
}

fn cal() -> String {
    #[cfg(test)]
    if let Ok(b) = std::env::var("GM_TEST_GOOGLE") {
        return format!("{b}/calendar/v3");
    }
    "https://www.googleapis.com/calendar/v3".into()
}

/// base + percent-encoded path segments + query (duplicate keys allowed).
pub(crate) fn url(base: &str, segs: &[&str], q: &[(&str, &str)]) -> String {
    let mut u = url::Url::parse(base).expect("base url");
    u.path_segments_mut().expect("base url path").pop_if_empty().extend(segs);
    if !q.is_empty() {
        u.query_pairs_mut().extend_pairs(q);
    }
    u.to_string()
}

/// Which OAuth client to use: run-time environment first, then what was built in. Both id and secret are required.
fn resolve_client(env_id: Option<String>, env_secret: Option<String>, built_id: &str, built_secret: &str) -> Result<(String, String)> {
    let pick = |env: Option<String>, built: &str| env.filter(|s| !s.is_empty()).unwrap_or_else(|| built.to_string());
    let (id, secret) = (pick(env_id, built_id), pick(env_secret, built_secret));
    if id.is_empty() || secret.is_empty() {
        bail!("Google sign-in isn't set up in this build of Graphite Mail (the Google client secret is missing).");
    }
    Ok((id, secret))
}

/// Whether this build can sign in to Google (it was built with the Google client secret, or one is set at run time).
pub fn sign_in_available() -> bool {
    client().is_ok()
}

fn client() -> Result<(String, String)> {
    resolve_client(std::env::var("GRAPHITE_GOOGLE_CLIENT_ID").ok(), std::env::var("GRAPHITE_GOOGLE_CLIENT_SECRET").ok(), CLIENT_ID, CLIENT_SECRET)
}

// ---------------------------------------------------------------- sign-in

#[derive(Deserialize)]
struct Tok {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    scope: Option<String>,
}

async fn token_request(form: &[(&str, &str)]) -> Result<Tok> {
    let resp = crate::http::client().post(TOKEN_URL).form(form).send().await?;
    if !resp.status().is_success() {
        let v: Value = resp.json().await.unwrap_or_default();
        return Err(match v["error"].as_str() {
            Some("invalid_grant") => anyhow!("Your Google sign-in has expired or was revoked. Remove the account in Settings and add it again."),
            _ => anyhow!("Google sign-in failed: {}", v["error_description"].as_str().or(v["error"].as_str()).unwrap_or("unknown error")),
        });
    }
    Ok(resp.json().await?)
}

/// Interactive Google sign-in. `open` is given the URL to show in the system browser.
pub async fn sign_in(open: impl FnOnce(&str) -> Result<()>) -> Result<auth::Account> {
    sign_in_with(open, false, None).await
}

/// Google sign-in that can also ask for permission to manage Gmail filters (mail rules).
pub async fn sign_in_with(open: impl FnOnce(&str) -> Result<()>, rules: bool, hint: Option<&str>) -> Result<auth::Account> {
    let (client_id, secret) = client()?;
    let scope = if rules { format!("{SCOPES} {FILTERS_SCOPE}") } else { SCOPES.to_string() };
    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let challenge = auth::b64(&<sha2::Sha256 as sha2::Digest>::digest(verifier.as_bytes()));
    let state = uuid::Uuid::new_v4().simple().to_string();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let redirect = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let mut u = url::Url::parse(AUTH_URL)?;
    u.query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &redirect)
        .append_pair("response_type", "code")
        .append_pair("scope", &scope)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent select_account");
    if let Some(h) = hint {
        u.query_pairs_mut().append_pair("login_hint", h);
    }
    open(u.as_str())?;

    let code = auth::wait_for_sign_in(&listener, &state).await?;

    let mut form = vec![
        ("client_id", client_id.as_str()),
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("code_verifier", verifier.as_str()),
    ];
    if !secret.is_empty() {
        form.push(("client_secret", secret.as_str()));
    }
    let tok = token_request(&form).await?;
    if !tok.scope.as_deref().unwrap_or_default().contains("gmail.modify") {
        bail!("Graphite Mail needs permission to read and manage your Gmail. Please try again and tick every box on the Google permission screen.");
    }
    if rules && !tok.scope.as_deref().unwrap_or_default().contains("gmail.settings.basic") {
        bail!("Rules need the \"manage your Gmail settings\" permission. Please try again and tick every box on the Google permission screen.");
    }
    let rt = tok.refresh_token.clone().ok_or_else(|| {
        anyhow!("Google didn't give Graphite Mail a long-lasting sign-in. Remove Graphite Mail at myaccount.google.com/permissions and try again.")
    })?;

    let me: Value = crate::http::client().get(USERINFO_URL).bearer_auth(&tok.access_token).send().await?.error_for_status()?.json().await?;
    let email = me["email"].as_str().ok_or_else(|| anyhow!("Google didn't tell us the account's email address."))?.to_string();
    let acc = auth::Account { display_name: me["name"].as_str().unwrap_or(&email).to_string(), email, provider: "google".into(), rules, contacts: false, personal: false, imap: None };
    auth::remember(&acc, &rt)?;
    auth::cache_token(&acc.email, &tok.access_token, tok.expires_in);
    Ok(acc)
}

/// Silent token refresh for a stored Google account.
pub async fn access_token(email: &str) -> Result<String> {
    let (client_id, secret) = client()?;
    let rt = auth::refresh_token_for(email)?;
    let mut form = vec![("client_id", client_id.as_str()), ("grant_type", "refresh_token"), ("refresh_token", rt.as_str())];
    if !secret.is_empty() {
        form.push(("client_secret", secret.as_str()));
    }
    let tok = token_request(&form).await.map_err(|e| anyhow!("{email}: {e}"))?;
    auth::cache_token(email, &tok.access_token, tok.expires_in);
    Ok(tok.access_token)
}

// ---------------------------------------------------------------- HTTP

/// Any status comes back as data, so callers can decide (the offline queue needs the raw status).
pub(crate) async fn call(method: Method, url: &str, body: Option<Value>) -> Result<(u16, Value)> {
    let token = auth::access_token().await?;
    let mut req = crate::http::client().request(method, url).bearer_auth(token);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await?;
    let status = resp.status().as_u16();
    let v = resp.json().await.unwrap_or(Value::Null);
    Ok((status, v))
}

fn api_error(status: u16, v: &Value) -> anyhow::Error {
    let msg = v["error"]["message"].as_str().unwrap_or("Google refused the request");
    if msg.contains("has not been used in project") || msg.contains("is disabled") {
        return anyhow!("Google {status}: {msg} (the Gmail/Calendar API must be switched on for the project that owns Graphite Mail's Google client)");
    }
    anyhow!("Google {status}: {msg}")
}

async fn get_ok(url: &str) -> Result<Value> {
    let (s, v) = call(Method::GET, url, None).await?;
    if (200..300).contains(&s) { Ok(v) } else { Err(api_error(s, &v)) }
}

async fn send_ok(method: Method, url: &str, body: Option<Value>) -> Result<Value> {
    let (s, v) = call(method, url, body).await?;
    if (200..300).contains(&s) { Ok(v) } else { Err(api_error(s, &v)) }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn state_get(account_id: &str, key: &str) -> Option<String> {
    storage::with_db(|db| Ok(db.query_row("SELECT value FROM account_state WHERE account_id=?1 AND key=?2", params![account_id, key], |r| r.get(0)).optional()?))
        .ok()
        .flatten()
}

fn state_set(account_id: &str, key: &str, value: &str) -> Result<()> {
    storage::with_db(|db| {
        db.execute(
            "INSERT INTO account_state(account_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(account_id,key) DO UPDATE SET value=excluded.value",
            params![account_id, key, value],
        )?;
        Ok(())
    })
}

fn state_clear(account_id: &str) -> Result<()> {
    storage::with_db(|db| Ok(db.execute("DELETE FROM account_state WHERE account_id=?1", [account_id]).map(|_| ())?))
}

// ---------------------------------------------------------------- labels = folders

const ARCHIVE: &str = "ALL_MAIL";

fn system_label(id: &str) -> Option<(&'static str, &'static str)> {
    Some(match id {
        "INBOX" => ("Inbox", "inbox"),
        "SENT" => ("Sent", "sentitems"),
        "DRAFT" => ("Drafts", "drafts"),
        "TRASH" => ("Trash", "deleteditems"),
        "SPAM" => ("Spam", "junkemail"),
        _ => return None,
    })
}

/// Gmail label id → local folder id, plus the local id of the synthetic Archive folder.
#[derive(Default, Clone)]
struct FolderMap {
    by_label: HashMap<String, String>,
    archive: String,
}

impl FolderMap {
    /// The folder a message lives in: Trash, Spam, Drafts, Inbox, Sent, then its first user label, else Archive.
    fn primary(&self, labels: &[String]) -> String {
        for l in ["TRASH", "SPAM", "DRAFT", "INBOX", "SENT"] {
            if labels.iter().any(|x| x == l) {
                if let Some(f) = self.by_label.get(l) {
                    return f.clone();
                }
            }
        }
        labels.iter().filter_map(|l| self.by_label.get(l)).next().cloned().unwrap_or_else(|| self.archive.clone())
    }
}

async fn label_detail(id: String) -> (String, Value) {
    let d = get_ok(&url(&gmail(), &["labels", &id], &[])).await.unwrap_or(Value::Null);
    (id, d)
}

async fn sync_labels(account_id: &str) -> Result<(FolderMap, usize)> {
    let list = get_ok(&url(&gmail(), &["labels"], &[])).await?;
    let all = list["labels"].as_array().cloned().unwrap_or_default();
    let keep: Vec<&Value> = all.iter().filter(|l| l["type"].as_str() == Some("user") || system_label(l["id"].as_str().unwrap_or_default()).is_some()).collect();
    let names: HashMap<String, String> = keep.iter().filter_map(|l| Some((l["name"].as_str()?.to_string(), l["id"].as_str()?.to_string()))).collect();

    // Counts need one request per label.
    let ids: Vec<String> = keep.iter().map(|l| l["id"].as_str().unwrap_or_default().to_string()).collect();
    let details: Vec<(String, Value)> = stream::iter(ids).map(label_detail).buffer_unordered(PARALLEL).collect().await;
    let details: HashMap<String, Value> = details.into_iter().collect();

    let mut map = FolderMap::default();
    let mut seen: HashSet<String> = HashSet::new();
    storage::with_db(|db| {
        let upsert = |rid: &str, parent: Option<&str>, name: &str, wk: Option<&str>, unread: i64, total: i64| -> Result<String> {
            db.execute(
                "INSERT INTO folders(id,account_id,remote_id,parent_remote_id,display_name,well_known_name,unread_count,total_count)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(account_id,remote_id) DO UPDATE SET display_name=excluded.display_name, parent_remote_id=excluded.parent_remote_id,
                   well_known_name=excluded.well_known_name, unread_count=excluded.unread_count, total_count=excluded.total_count",
                params![uuid::Uuid::new_v4().to_string(), account_id, rid, parent, name, wk, unread, total],
            )?;
            Ok(db.query_row("SELECT id FROM folders WHERE account_id=?1 AND remote_id=?2", params![account_id, rid], |r| r.get(0))?)
        };
        for l in &keep {
            let rid = l["id"].as_str().unwrap_or_default();
            let full = l["name"].as_str().unwrap_or(rid);
            let d = details.get(rid).cloned().unwrap_or(Value::Null);
            let (unread, total) = (d["messagesUnread"].as_i64().unwrap_or(0), d["messagesTotal"].as_i64().unwrap_or(0));
            let local = if let Some((display, wk)) = system_label(rid) {
                upsert(rid, None, display, Some(wk), unread, total)?
            } else {
                // "Family/Trips" is a label inside "Family" (when that label exists).
                let parent = full.rsplit_once('/').and_then(|(p, _)| names.get(p)).map(String::as_str);
                let display = if parent.is_some() { full.rsplit('/').next().unwrap_or(full) } else { full };
                upsert(rid, parent, display, None, unread, total)?
            };
            map.by_label.insert(rid.to_string(), local);
            seen.insert(rid.to_string());
        }
        map.archive = upsert(ARCHIVE, None, "Archive", Some("archive"), 0, 0)?;
        seen.insert(ARCHIVE.to_string());

        // Labels deleted elsewhere: keep their messages (they still exist in Gmail), drop only the folder.
        let stale: Vec<String> = {
            let mut st = db.prepare("SELECT id, remote_id FROM folders WHERE account_id=?1")?;
            let rows = st.query_map([account_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.filter_map(|r| r.ok()).filter(|(_, rid)| !seen.contains(rid)).map(|(id, _)| id).collect()
        };
        for id in &stale {
            db.execute("UPDATE messages SET folder_id=?1 WHERE folder_id=?2", params![map.archive, id])?;
            db.execute("DELETE FROM folders WHERE id=?1", [id])?;
        }
        Ok(())
    })?;
    Ok((map, keep.len()))
}

// ---------------------------------------------------------------- messages

pub struct Inline {
    pub cid: String,
    pub mime: String,
    pub name: String,
    pub bytes: Vec<u8>,
}

pub struct Parsed {
    pub subject: Option<String>,
    pub from_name: Option<String>,
    pub from_addr: Option<String>,
    pub to_json: String,
    pub cc_json: String,
    pub message_id: Option<String>,
    pub html: String,
    pub inline: Vec<Inline>,
    /// (name, mime, size) of real attachments
    pub files: Vec<(String, String, usize)>,
    /// The bytes of `files`, only when asked for (parse_raw_full with keep_files).
    pub file_bytes: Vec<Vec<u8>>,
}

pub(crate) fn people(a: Option<&mail_parser::Address>) -> String {
    let list: Vec<Value> = a
        .map(|a| a.iter().map(|x| json!({ "name": x.name(), "address": x.address() })).collect())
        .unwrap_or_default();
    Value::Array(list).to_string()
}

/// Turns raw RFC 5322 mail into what the app stores. Handles encoded headers, charsets, multipart, inline pictures.
pub fn parse_raw(raw: &[u8]) -> Result<Parsed> {
    parse_raw_full(raw, false)
}

pub fn parse_raw_full(raw: &[u8], keep_files: bool) -> Result<Parsed> {
    let mut file_bytes = Vec::new();
    let msg = MessageParser::default().parse(raw).ok_or_else(|| anyhow!("Could not read a message from Gmail"))?;
    let from = msg.from().and_then(|a| a.first());
    let mut inline = Vec::new();
    let mut files = Vec::new();
    for part in msg.attachments() {
        let mime = part.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream"))).unwrap_or_else(|| "application/octet-stream".into());
        let name = part.attachment_name().unwrap_or("attachment").to_string();
        match part.content_id() {
            Some(cid) if mime.starts_with("image/") => inline.push(Inline {
                cid: cid.trim_matches(|c| c == '<' || c == '>').to_string(),
                mime,
                name,
                bytes: part.contents().to_vec(),
            }),
            _ => {
                files.push((name, mime, part.contents().len()));
                if keep_files {
                    file_bytes.push(part.contents().to_vec());
                }
            }
        }
    }
    Ok(Parsed {
        subject: msg.subject().map(String::from),
        from_name: from.and_then(|f| f.name()).filter(|n| !n.is_empty()).map(String::from),
        from_addr: from.and_then(|f| f.address()).map(String::from),
        to_json: people(msg.to()),
        cc_json: people(msg.cc()),
        message_id: msg.message_id().map(String::from),
        html: msg.body_html(0).map(|c| c.into_owned()).unwrap_or_default(),
        inline,
        files,
        file_bytes,
    })
}

/// Downloads one attachment of a Gmail message (found by name and size in the full message).
pub async fn download_attachment(message_remote_id: &str, name: &str, size: i64) -> Result<Vec<u8>> {
    let v = get_ok(&url(&gmail(), &["messages", message_remote_id], &[("format", "raw")])).await?;
    let p = parse_raw_full(&unb64(v["raw"].as_str().unwrap_or_default())?, true)?;
    let idx = p.files.iter().position(|(n, _, s)| n == name && *s as i64 == size).or_else(|| p.files.iter().position(|(n, _, _)| n == name));
    idx.and_then(|i| p.file_bytes.get(i).cloned()).ok_or_else(|| anyhow!("That attachment is no longer in the message."))
}

/// An attachment waiting to be sent: (file name, mime type, bytes).
pub type OutFile = (String, String, Vec<u8>);

/// What Gmail says about a message besides its text.
struct Meta {
    id: String,
    thread_id: Option<String>,
    labels: Vec<String>,
    snippet: String,
    received: i64,
    history_id: String,
}

fn meta_of(v: &Value) -> Meta {
    Meta {
        id: v["id"].as_str().unwrap_or_default().to_string(),
        thread_id: v["threadId"].as_str().map(String::from),
        labels: v["labelIds"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
        snippet: v["snippet"].as_str().unwrap_or_default().to_string(),
        received: v["internalDate"].as_str().and_then(|s| s.parse::<i64>().ok()).map(|ms| ms / 1000).unwrap_or_else(now),
        history_id: v["historyId"].as_str().unwrap_or_default().to_string(),
    }
}

fn unb64(s: &str) -> Result<Vec<u8>> {
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim_end_matches('='))?)
}

/// Stores (or refreshes) one message. Messages with unsent local changes are left alone.
fn store_message(account_id: &str, map: &FolderMap, meta: &Meta, parsed: Option<&Parsed>) -> Result<()> {
    let folder = map.primary(&meta.labels);
    let unread = meta.labels.iter().any(|l| l == "UNREAD");
    let starred = meta.labels.iter().any(|l| l == "STARRED");
    let draft = meta.labels.iter().any(|l| l == "DRAFT");
    let existing: Option<(String, String)> = storage::with_db(|db| {
        Ok(db.query_row("SELECT id, sync_status FROM messages WHERE remote_id=?1 AND account_id=?2", params![meta.id, account_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
    })?;
    if matches!(&existing, Some((_, s)) if s != "SYNCED") {
        return Ok(()); // the user changed it here; our queued change goes out first
    }

    let Some(p) = parsed else {
        // Labels/flags only (a "minimal" update).
        let Some((id, _)) = existing else { return Ok(()) };
        return storage::with_db(|db| {
            db.execute(
                "UPDATE messages SET folder_id=?1, is_read=?2, is_flagged=?3, is_draft=?4, snippet=?5, change_key=?6 WHERE id=?7",
                params![folder, !unread, starred, draft, meta.snippet, meta.history_id, id],
            )?;
            set_labels(db, &id, map, &meta.labels)?;
            Ok(())
        });
    };

    let body_hash = sync::write_blob(p.html.as_bytes(), "html")?;
    let local_id = existing.map(|(id, _)| id).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let inline_hashes: Vec<String> = p.inline.iter().map(|i| sync::write_blob(&i.bytes, "bin")).collect::<Result<_>>()?;
    let text = sync::strip_html(&p.html);
    storage::with_db(|db| {
        let tx = db.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO messages(id,account_id,folder_id,remote_id,internet_message_id,conversation_id,subject,sender_name,sender_address,
                to_recipients_json,cc_recipients_json,snippet,received_at,is_read,is_flagged,is_draft,has_attachments,change_key,
                body_blob_hash,body_cached,inline_done)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,1,1)
             ON CONFLICT(remote_id) DO UPDATE SET folder_id=excluded.folder_id, subject=excluded.subject, sender_name=excluded.sender_name,
                sender_address=excluded.sender_address, to_recipients_json=excluded.to_recipients_json, cc_recipients_json=excluded.cc_recipients_json,
                snippet=excluded.snippet, is_read=excluded.is_read, is_flagged=excluded.is_flagged, is_draft=excluded.is_draft,
                has_attachments=excluded.has_attachments, change_key=excluded.change_key, body_blob_hash=excluded.body_blob_hash,
                body_cached=1, inline_done=1",
            params![
                local_id, account_id, folder, meta.id, p.message_id, meta.thread_id, p.subject, p.from_name, p.from_addr, p.to_json, p.cc_json,
                meta.snippet, meta.received, !unread, starred, draft, !p.files.is_empty(), meta.history_id, body_hash
            ],
        )?;
        tx.execute("DELETE FROM attachments WHERE message_id=?1", [&local_id])?;
        for (i, h) in p.inline.iter().zip(&inline_hashes) {
            tx.execute(
                "INSERT INTO attachments(id,message_id,remote_attachment_id,filename,mime_type,size_bytes,content_id,is_inline,blob_hash,is_downloaded)
                 VALUES(?1,?2,NULL,?3,?4,?5,?6,1,?7,1)",
                params![uuid::Uuid::new_v4().to_string(), local_id, i.name, i.mime, i.bytes.len() as i64, i.cid, h],
            )?;
        }
        for (name, mime, size) in &p.files {
            tx.execute(
                "INSERT INTO attachments(id,message_id,filename,mime_type,size_bytes,is_inline,is_downloaded) VALUES(?1,?2,?3,?4,?5,0,0)",
                params![uuid::Uuid::new_v4().to_string(), local_id, name, mime, *size as i64],
            )?;
        }
        set_labels(&tx, &local_id, map, &meta.labels)?;
        tx.execute("DELETE FROM messages_fts WHERE message_id=?1", [&local_id])?;
        tx.execute(
            "INSERT INTO messages_fts(message_id,subject,sender_name,sender_address,body_plaintext) VALUES(?1,?2,?3,?4,?5)",
            params![local_id, p.subject.clone().unwrap_or_default(), p.from_name.clone().unwrap_or_default(), p.from_addr.clone().unwrap_or_default(), text],
        )?;
        tx.commit()?;
        Ok(())
    })
}

fn set_labels(db: &rusqlite::Connection, message_id: &str, map: &FolderMap, labels: &[String]) -> Result<()> {
    db.execute("DELETE FROM message_labels WHERE message_id=?1", [message_id])?;
    // In Gmail, trashed mail doesn't show under its other labels.
    if labels.iter().any(|l| l == "TRASH") {
        return Ok(());
    }
    for l in labels {
        if let Some(f) = map.by_label.get(l) {
            db.execute("INSERT OR IGNORE INTO message_labels(message_id,folder_id) VALUES(?1,?2)", params![message_id, f])?;
        }
    }
    Ok(())
}

async fn fetch_raw(id: String) -> (String, Result<(Meta, Parsed)>) {
    let r = async {
        let v = get_ok(&url(&gmail(), &["messages", &id], &[("format", "raw")])).await?;
        let meta = meta_of(&v);
        let parsed = parse_raw(&unb64(v["raw"].as_str().unwrap_or_default())?)?;
        Ok((meta, parsed))
    }
    .await;
    (id, r)
}

async fn fetch_minimal(id: &str) -> Result<Option<Meta>> {
    let (s, v) = call(Method::GET, &url(&gmail(), &["messages", id], &[("format", "minimal")]), None).await?;
    match s {
        404 => Ok(None),
        200..=299 => Ok(Some(meta_of(&v))),
        _ => Err(api_error(s, &v)),
    }
}

fn is_local(account_id: &str, remote: &str) -> bool {
    storage::with_db(|db| Ok(db.query_row("SELECT COUNT(*) FROM messages WHERE remote_id=?1 AND account_id=?2 AND body_cached=1", params![remote, account_id], |r| r.get::<_, i64>(0))? > 0)).unwrap_or(false)
}

fn delete_local(account_id: &str, remote: &str) -> Result<()> {
    storage::with_db(|db| {
        db.execute(
            "DELETE FROM messages_fts WHERE message_id IN (SELECT id FROM messages WHERE remote_id=?1 AND account_id=?2 AND sync_status='SYNCED')",
            params![remote, account_id],
        )?;
        db.execute("DELETE FROM messages WHERE remote_id=?1 AND account_id=?2 AND sync_status='SYNCED'", params![remote, account_id])?;
        Ok(())
    })
}

/// Downloads whole messages, a few at a time, and stores each as it arrives.
async fn download(account_id: &str, map: &FolderMap, ids: Vec<String>) -> Result<usize> {
    let mut n = 0;
    let mut results = stream::iter(ids).map(fetch_raw).buffer_unordered(PARALLEL);
    while let Some((id, r)) = results.next().await {
        match r {
            Ok((meta, parsed)) => {
                store_message(account_id, map, &meta, Some(&parsed))?;
                n += 1;
            }
            Err(e) => eprintln!("[graphite] gmail message {id} skipped: {e}"),
        }
    }
    Ok(n)
}

async fn initial_sync(account_id: &str, map: &FolderMap, history_at_start: &str) -> Result<usize> {
    let q = format!("newer_than:{INITIAL_DAYS}d");
    let mut ids: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut qs = vec![("q", q.as_str()), ("includeSpamTrash", "true"), ("maxResults", "500")];
        if let Some(t) = &token {
            qs.push(("pageToken", t.as_str()));
        }
        let page = get_ok(&url(&gmail(), &["messages"], &qs)).await?;
        ids.extend(page["messages"].as_array().map(|a| a.iter().filter_map(|m| m["id"].as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default());
        token = page["nextPageToken"].as_str().map(String::from);
        if token.is_none() || ids.len() >= INITIAL_MAX {
            break;
        }
    }
    ids.truncate(INITIAL_MAX);
    ids.retain(|id| !is_local(account_id, id)); // resume after an interrupted first sync
    let n = download(account_id, map, ids).await?;
    state_set(account_id, "history_id", history_at_start)?;
    Ok(n)
}

/// Applies everything that changed since `since`. Returns None when Gmail no longer remembers that point.
async fn incremental_sync(account_id: &str, map: &FolderMap, since: &str) -> Result<Option<(usize, String)>> {
    let (mut added, mut changed, mut deleted) = (Vec::<String>::new(), Vec::<String>::new(), HashSet::<String>::new());
    let mut newest = since.to_string();
    let mut token: Option<String> = None;
    loop {
        let mut qs = vec![
            ("startHistoryId", since),
            ("historyTypes", "messageAdded"),
            ("historyTypes", "messageDeleted"),
            ("historyTypes", "labelAdded"),
            ("historyTypes", "labelRemoved"),
            ("maxResults", "500"),
        ];
        if let Some(t) = &token {
            qs.push(("pageToken", t.as_str()));
        }
        let (s, v) = call(Method::GET, &url(&gmail(), &["history"], &qs), None).await?;
        if s == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&s) {
            return Err(api_error(s, &v));
        }
        let id_of = |m: &Value| m["message"]["id"].as_str().map(String::from);
        for h in v["history"].as_array().cloned().unwrap_or_default() {
            for key in ["messagesAdded"] {
                added.extend(h[key].as_array().into_iter().flatten().filter_map(id_of));
            }
            for key in ["labelsAdded", "labelsRemoved"] {
                changed.extend(h[key].as_array().into_iter().flatten().filter_map(id_of));
            }
            deleted.extend(h["messagesDeleted"].as_array().into_iter().flatten().filter_map(id_of));
        }
        if let Some(h) = v["historyId"].as_str() {
            newest = h.to_string();
        }
        token = v["nextPageToken"].as_str().map(String::from);
        if token.is_none() {
            break;
        }
    }
    let mut n = 0;
    for id in &deleted {
        delete_local(account_id, id)?;
        n += 1;
    }
    let mut seen = HashSet::new();
    let full: Vec<String> = added.into_iter().filter(|id| !deleted.contains(id) && seen.insert(id.clone()) && !is_local(account_id, id)).collect();
    n += download(account_id, map, full).await?;
    for id in changed.into_iter().filter(|id| !deleted.contains(id) && seen.insert(id.clone())) {
        match fetch_minimal(&id).await? {
            Some(meta) if is_local(account_id, &id) => {
                store_message(account_id, map, &meta, None)?;
                n += 1;
            }
            Some(_) => n += download(account_id, map, vec![id]).await?, // labelled mail we never downloaded
            None => delete_local(account_id, &id)?,
        }
    }
    Ok(Some((n, newest)))
}

/// Archive has no counts of its own on the server; count what we hold.
fn count_archive(account_id: &str, map: &FolderMap) -> Result<()> {
    storage::with_db(|db| {
        db.execute(
            "UPDATE folders SET total_count=(SELECT COUNT(*) FROM messages WHERE folder_id=?1), unread_count=(SELECT COUNT(*) FROM messages WHERE folder_id=?1 AND is_read=0) WHERE id=?1 AND account_id=?2",
            params![map.archive, account_id],
        )?;
        Ok(())
    })
}

/// One full pass for a Gmail account (runs inside auth::in_account for that account).
pub async fn sync_account(account_id: &str) -> Result<sync::SyncReport> {
    let mut rep = sync::SyncReport::default();
    let profile = get_ok(&url(&gmail(), &["profile"], &[])).await?;
    let (map, folders) = sync_labels(account_id).await?;
    rep.folders = folders;

    let first_time = state_get(account_id, "history_id").is_none();
    match state_get(account_id, "history_id") {
        None => rep.messages_changed = initial_sync(account_id, &map, profile["historyId"].as_str().unwrap_or("1")).await?,
        Some(since) => match incremental_sync(account_id, &map, &since).await? {
            Some((n, newest)) => {
                rep.messages_changed = n;
                state_set(account_id, "history_id", &newest)?;
            }
            None => {
                // History too old (Gmail keeps about a week): start over, keeping what we have.
                state_clear(account_id)?;
                rep.messages_changed = initial_sync(account_id, &map, profile["historyId"].as_str().unwrap_or("1")).await?;
            }
        },
    }
    count_archive(account_id, &map)?;
    if first_time {
        let _ = crate::notify::collect_new(); // the first download must not flood the desktop with notifications
    }
    match sync_calendar(account_id).await {
        Ok(n) => rep.events = n,
        Err(e) => {
            eprintln!("[graphite] google calendar sync failed: {e}");
            rep.calendar_error = Some(e.to_string());
        }
    }
    Ok(rep)
}

// ---------------------------------------------------------------- sending

/// Builds the RFC 5322 message Gmail sends. `reply` = (Message-ID of the original, its References).
/// `bcc` is written as a `Bcc:` header: only pass it where the provider turns that header into envelope recipients and
/// removes it before delivery (the Gmail API does). SMTP callers pass none and put blind copies in the envelope instead.
pub fn build_raw(from: (&str, &str), to: &[String], cc: &[String], bcc: &[String], subject: &str, html: &str, reply: Option<(&str, &str)>, files: &[OutFile]) -> Result<Vec<u8>> {
    use mail_builder::MessageBuilder;
    use mail_builder::headers::address::Address;
    fn addrs(l: &[String]) -> Address<'_> {
        Address::from(l.iter().map(|a| Address::new_address(None::<&str>, a.as_str())).collect::<Vec<_>>())
    }
    // No display name: just the address (not an empty "" name).
    let sender = if from.0.trim().is_empty() { Address::new_address(None::<&str>, from.1) } else { Address::from(from) };
    let mut b = MessageBuilder::new().from(sender).to(addrs(to)).subject(subject).html_body(html).text_body(sync::strip_html(html));
    if !cc.is_empty() {
        b = b.cc(addrs(cc));
    }
    if !bcc.is_empty() {
        b = b.bcc(addrs(bcc));
    }
    for (name, mime, bytes) in files {
        b = b.attachment(mime.clone(), name.clone(), bytes.clone());
    }
    if let Some((id, refs)) = reply {
        let id = id.trim_matches(|c| c == '<' || c == '>');
        b = b.in_reply_to(id);
        let chain = format!("{} {}", refs.trim(), id).trim().to_string();
        let ids: Vec<String> = chain.split_whitespace().map(|s| s.trim_matches(|c| c == '<' || c == '>').to_string()).collect();
        b = b.references(ids);
    }
    Ok(b.write_to_vec()?)
}

async fn send_message(entity: &str, rid: Option<&str>, p: &Value) -> Result<(u16, Value)> {
    let (name, email): (String, String) = storage::with_db(|db| {
        Ok(db
            .query_row(
                "SELECT display_name, email FROM accounts WHERE lower(email)=lower(?1)",
                [auth::active_email().unwrap_or_default()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(anyhow!("Unknown sending account"))?)
    })?;
    let list = |k: &str| p[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default();
    // Replies join the original conversation: same thread, In-Reply-To/References headers.
    let original: Option<(Option<String>, Option<String>)> = match (p["mode"].as_str(), rid) {
        (Some("reply" | "replyAll"), Some(_)) => storage::with_db(|db| {
            Ok(db.query_row("SELECT internet_message_id, conversation_id FROM messages WHERE id=?1", [entity], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
        })?,
        _ => None,
    };
    let reply = original.as_ref().and_then(|(mid, _)| mid.as_deref()).map(|m| (m, ""));
    // Attachments chosen in the editor are kept in the blob store until the message goes out.
    let mut files: Vec<OutFile> = vec![];
    for a in p["attachments"].as_array().cloned().unwrap_or_default() {
        let bytes = sync::read_blob(a["blob"].as_str().unwrap_or_default(), "bin")?;
        files.push((a["name"].as_str().unwrap_or("attachment").to_string(), a["mime"].as_str().unwrap_or("application/octet-stream").to_string(), bytes));
    }
    // A forward carries the original's attachments (Gmail doesn't do that by itself).
    if let (Some("forward"), Some(r)) = (p["mode"].as_str(), rid) {
        let (s, v) = call(Method::GET, &url(&gmail(), &["messages", r], &[("format", "raw")]), None).await?;
        if !(200..300).contains(&s) {
            return Ok((s, v));
        }
        let orig = parse_raw_full(&unb64(v["raw"].as_str().unwrap_or_default())?, true)?;
        for ((n, m, _), b) in orig.files.into_iter().zip(orig.file_bytes) {
            files.push((n, m, b));
        }
    }
    // The Gmail API takes recipients from the headers only; Gmail removes the Bcc header from what it delivers.
    let raw = build_raw((&name, &email), &list("to"), &list("cc"), &list("bcc"), p["subject"].as_str().unwrap_or_default(), p["html"].as_str().unwrap_or_default(), reply, &files)?;
    let mut body = json!({ "raw": auth::b64(&raw) });
    if let Some((_, Some(thread))) = &original {
        body["threadId"] = json!(thread);
    }
    call(Method::POST, &url(&gmail(), &["messages", "send"], &[]), Some(body)).await
}

// ---------------------------------------------------------------- offline queue operations

fn modify_body(add: &[&str], remove: &[&str]) -> Value {
    json!({ "addLabelIds": add, "removeLabelIds": remove })
}

/// Runs one queued operation against Gmail / Google Calendar. Same contract as the Microsoft path:
/// returns the HTTP status and a (Graph-shaped, where the caller reads it) body.
pub async fn apply(op: &str, entity: &str, rid: Option<&str>, p: &Value) -> Result<(u16, Value)> {
    let g = gmail();
    let modify = |id: &str, add: &[&str], remove: &[&str]| {
        let u = url(&g, &["messages", id, "modify"], &[]);
        let b = modify_body(add, remove);
        async move { call(Method::POST, &u, Some(b)).await }
    };
    match (op, rid) {
        ("SET_READ", Some(r)) => {
            if p["read"].as_bool() == Some(true) { modify(r, &[], &["UNREAD"]).await } else { modify(r, &["UNREAD"], &[]).await }
        }
        ("SET_FLAG", Some(r)) => {
            if p["flagged"].as_bool() == Some(true) { modify(r, &["STARRED"], &[]).await } else { modify(r, &[], &["STARRED"]).await }
        }
        ("MOVE", Some(r)) => {
            let (dest, src) = (p["destination"].as_str().unwrap_or_default(), p["source"].as_str().unwrap_or_default());
            if src == "TRASH" && dest != "TRASH" {
                let (s, v) = call(Method::POST, &url(&g, &["messages", r, "untrash"], &[]), None).await?;
                if !(200..300).contains(&s) {
                    return Ok((s, v));
                }
            }
            match dest {
                "TRASH" => call(Method::POST, &url(&g, &["messages", r, "trash"], &[]), None).await,
                ARCHIVE => modify(r, &[], &["INBOX", "SPAM"]).await,
                "INBOX" => modify(r, &["INBOX"], &["SPAM"]).await,
                "SPAM" => modify(r, &["SPAM"], &["INBOX"]).await,
                "SENT" | "DRAFT" => Ok((400, json!({ "error": { "message": "Messages can't be moved into Sent or Drafts." } }))),
                label => {
                    // Filing under a label also takes the message out of the Inbox (and out of its previous label).
                    let mut remove = vec!["INBOX", "SPAM"];
                    if !matches!(src, "" | "INBOX" | "SPAM" | "TRASH" | "SENT" | "DRAFT" | ARCHIVE) {
                        remove.push(src);
                    }
                    modify(r, &[label], &remove).await
                }
            }
        }
        // Copying is just one more label; Gmail keeps a single message.
        ("COPY", Some(r)) => modify(r, &[p["destination"].as_str().unwrap_or_default()], &[]).await,
        // The app only asks for Gmail's "gmail.modify" permission, which cannot erase mail for good;
        // Gmail empties the Trash by itself after 30 days.
        ("DELETE_HARD", _) => Ok((200, Value::Null)),
        ("SEND", _) => send_message(entity, rid, p).await,
        ("RSVP", Some(r)) => rsvp(r, p["response"].as_str().unwrap_or("accept")).await,
        ("CREATE_EVENT", _) => create_event(p).await,
        ("DELETE_EVENT", Some(r)) => {
            let (cal_id, ev_id) = split_event(r)?;
            call(Method::DELETE, &url(&cal(), &["calendars", &cal_id, "events", &ev_id], &[("sendUpdates", "all")]), None).await
        }
        (_, None) => Ok((404, Value::Null)),
        _ => Ok((400, Value::Null)),
    }
}

// ---------------------------------------------------------------- folders (labels)

fn label_path(db: &rusqlite::Connection, local_id: &str) -> Result<(String, String, String)> {
    // (remote id, full "A/B/C" name, account id)
    let (rid, name, parent, aid): (String, String, Option<String>, String) = db
        .query_row("SELECT remote_id, display_name, parent_remote_id, account_id FROM folders WHERE id=?1", [local_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?
        .ok_or(anyhow!("That folder no longer exists."))?;
    let mut full = name;
    let mut cur = parent;
    while let Some(p) = cur {
        let row: Option<(String, Option<String>)> = db
            .query_row("SELECT display_name, parent_remote_id FROM folders WHERE account_id=?1 AND remote_id=?2", params![aid, p], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        match row {
            Some((n, pp)) => {
                full = format!("{n}/{full}");
                cur = pp;
            }
            None => break,
        }
    }
    Ok((rid, full, aid))
}

fn clean_label(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        bail!("Enter a folder name.");
    }
    if n.contains('/') {
        bail!("A label name can't contain “/”.");
    }
    Ok(n.to_string())
}

pub async fn create_label(name: &str, parent: Option<&str>, account_id: &str) -> Result<String> {
    let name = clean_label(name)?;
    let (full, parent_rid) = match parent {
        Some(p) => {
            let (prid, pfull, _) = storage::with_db(|db| label_path(db, p))?;
            (format!("{pfull}/{name}"), Some(prid))
        }
        None => (name.clone(), None),
    };
    let v = send_ok(Method::POST, &url(&gmail(), &["labels"], &[]), Some(json!({ "name": full, "labelListVisibility": "labelShow", "messageListVisibility": "show" }))).await?;
    let rid = v["id"].as_str().ok_or(anyhow!("Unexpected reply from Google"))?.to_string();
    let id = uuid::Uuid::new_v4().to_string();
    storage::with_db(|db| {
        db.execute(
            "INSERT INTO folders(id,account_id,remote_id,parent_remote_id,display_name,unread_count,total_count) VALUES(?1,?2,?3,?4,?5,0,0)
             ON CONFLICT(account_id,remote_id) DO NOTHING",
            params![id, account_id, rid, parent_rid, name],
        )?;
        Ok(db.query_row("SELECT id FROM folders WHERE account_id=?1 AND remote_id=?2", params![account_id, rid], |r| r.get(0))?)
    })
}

/// Local ids of a folder and everything below it.
fn descendants(db: &rusqlite::Connection, local_id: &str) -> Result<Vec<String>> {
    let (rid, aid): (String, String) = db.query_row("SELECT remote_id, account_id FROM folders WHERE id=?1", [local_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut out = vec![local_id.to_string()];
    let mut queue = vec![rid];
    while let Some(p) = queue.pop() {
        let mut st = db.prepare("SELECT id, remote_id FROM folders WHERE account_id=?1 AND parent_remote_id=?2")?;
        let kids: Vec<(String, String)> = st.query_map(params![aid, p], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|r| r.ok()).collect();
        for (id, rid) in kids {
            out.push(id);
            queue.push(rid);
        }
    }
    Ok(out)
}

pub async fn rename_label(local_id: &str, name: &str) -> Result<()> {
    let name = clean_label(name)?;
    let (rid, old_full, aid, builtin) = storage::with_db(|db| {
        let wk: Option<String> = db.query_row("SELECT well_known_name FROM folders WHERE id=?1", [local_id], |r| r.get(0))?;
        let (rid, full, aid) = label_path(db, local_id)?;
        Ok((rid, full, aid, wk.is_some()))
    })?;
    if builtin {
        bail!("Built-in folders like Inbox can't be renamed.");
    }
    let new_full = match old_full.rsplit_once('/') {
        Some((parent, _)) => format!("{parent}/{name}"),
        None => name.clone(),
    };
    send_ok(Method::PATCH, &url(&gmail(), &["labels", &rid], &[]), Some(json!({ "name": new_full }))).await?;
    // Labels inside it carry the full path in their name, so they are renamed along.
    let kids: Vec<(String, String)> = storage::with_db(|db| {
        let ids = descendants(db, local_id)?;
        let mut out = vec![];
        for id in ids.into_iter().skip(1) {
            let (krid, kfull, _) = label_path(db, &id)?;
            out.push((krid, kfull));
        }
        Ok(out)
    })?;
    for (krid, kfull) in kids {
        let renamed = format!("{new_full}{}", &kfull[old_full.len()..]);
        send_ok(Method::PATCH, &url(&gmail(), &["labels", &krid], &[]), Some(json!({ "name": renamed }))).await?;
    }
    let _ = aid;
    storage::with_db(|db| Ok(db.execute("UPDATE folders SET display_name=?1 WHERE id=?2", params![name, local_id]).map(|_| ())?))
}

pub async fn delete_label(local_id: &str) -> Result<()> {
    let (ids, archive): (Vec<(String, String)>, String) = storage::with_db(|db| {
        let wk: Option<String> = db.query_row("SELECT well_known_name FROM folders WHERE id=?1", [local_id], |r| r.get(0))?;
        if wk.is_some() {
            bail!("Built-in folders like Inbox can't be deleted.");
        }
        let (_, _, aid) = label_path(db, local_id)?;
        let archive: String = db.query_row("SELECT id FROM folders WHERE account_id=?1 AND remote_id=?2", params![aid, ARCHIVE], |r| r.get(0))?;
        let mut v = vec![];
        for id in descendants(db, local_id)? {
            let rid: String = db.query_row("SELECT remote_id FROM folders WHERE id=?1", [&id], |r| r.get(0))?;
            v.push((id, rid));
        }
        Ok((v, archive))
    })?;
    for (_, rid) in &ids {
        send_ok(Method::DELETE, &url(&gmail(), &["labels", rid], &[]), None).await?;
    }
    // The messages stay in Gmail; here they fall back to Archive until the next sync sorts them again.
    storage::with_db(|db| {
        for (id, _) in &ids {
            db.execute("UPDATE messages SET folder_id=?1 WHERE folder_id=?2", params![archive, id])?;
            db.execute("DELETE FROM folders WHERE id=?1", [id])?;
        }
        Ok(())
    })
}

// ---------------------------------------------------------------- calendar

/// Event/calendar ids are stored as "account|calendarId[|eventId]" so shared calendars never collide between accounts.
fn split_event(remote: &str) -> Result<(String, String)> {
    let mut it = remote.splitn(3, '|');
    let (_, cal, ev) = (it.next(), it.next(), it.next());
    match (cal, ev) {
        (Some(c), Some(e)) => Ok((c.to_string(), e.to_string())),
        _ => bail!("Unrecognised event id"),
    }
}

fn cal_part(remote: &str) -> &str {
    remote.split_once('|').map(|x| x.1).unwrap_or(remote)
}

fn when(v: &Value) -> Option<(i64, bool, String)> {
    if let Some(dt) = v["dateTime"].as_str() {
        let t = chrono::DateTime::parse_from_rfc3339(dt).ok()?.timestamp();
        return Some((t, false, v["timeZone"].as_str().unwrap_or("UTC").to_string()));
    }
    let d = chrono::NaiveDate::parse_from_str(v["date"].as_str()?, "%Y-%m-%d").ok()?;
    Some((d.and_hms_opt(0, 0, 0)?.and_utc().timestamp(), true, "UTC".into()))
}

fn response_word(r: &str) -> &'static str {
    match r {
        "accepted" => "accepted",
        "tentative" => "tentativelyAccepted",
        "declined" => "declined",
        _ => "notResponded",
    }
}

pub struct EventRow {
    pub remote_id: String,
    pub ical_uid: String,
    pub series: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub meeting_url: Option<String>,
    pub start: i64,
    pub end: i64,
    pub tz: String,
    pub all_day: bool,
    pub organizer_json: String,
    pub attendees_json: String,
    pub my_response: String,
    pub change_key: Option<String>,
}

/// Google Calendar event → the app's event row. None for cancelled/undated events.
pub fn event_row(email: &str, cal_id: &str, e: &Value) -> Option<EventRow> {
    if e["status"].as_str() == Some("cancelled") {
        return None;
    }
    let (start, all_day, tz) = when(&e["start"])?;
    let (end, _, _) = when(&e["end"])?;
    let id = e["id"].as_str()?;
    let attendees: Vec<Value> = e["attendees"]
        .as_array()
        .map(|a| a.iter().map(|x| json!({ "name": x["displayName"], "email": x["email"], "response": response_word(x["responseStatus"].as_str().unwrap_or("")) })).collect())
        .unwrap_or_default();
    let mine = e["attendees"].as_array().and_then(|a| a.iter().find(|x| x["self"].as_bool() == Some(true)));
    let my_response = if e["organizer"]["self"].as_bool() == Some(true) || (e["attendees"].is_null() && e["creator"]["self"].as_bool() == Some(true)) {
        "organizer".to_string()
    } else {
        mine.map(|m| response_word(m["responseStatus"].as_str().unwrap_or("")).to_string()).unwrap_or_else(|| "none".into())
    };
    let meeting_url = e["hangoutLink"].as_str().map(String::from).or_else(|| {
        e["conferenceData"]["entryPoints"].as_array()?.iter().find(|p| p["entryPointType"].as_str() == Some("video"))?["uri"].as_str().map(String::from)
    });
    Some(EventRow {
        remote_id: format!("{email}|{cal_id}|{id}"),
        ical_uid: e["iCalUID"].as_str().unwrap_or(id).to_string(),
        series: e["recurringEventId"].as_str().map(|s| format!("{email}|{cal_id}|{s}")),
        title: e["summary"].as_str().unwrap_or("(no title)").to_string(),
        description: e["description"].as_str().map(String::from),
        location: e["location"].as_str().filter(|s| !s.is_empty()).map(String::from),
        meeting_url,
        start,
        end,
        tz,
        all_day,
        organizer_json: json!({ "name": e["organizer"]["displayName"], "address": e["organizer"]["email"] }).to_string(),
        attendees_json: Value::Array(attendees).to_string(),
        my_response,
        change_key: e["etag"].as_str().map(String::from),
    })
}

async fn sync_calendar(account_id: &str) -> Result<usize> {
    let email: String = storage::with_db(|db| Ok(db.query_row("SELECT email FROM accounts WHERE id=?1", [account_id], |r| r.get(0))?))?;
    let list = get_ok(&url(&cal(), &["users", "me", "calendarList"], &[("minAccessRole", "reader")])).await?;
    let mut total = 0;
    let mut kept = Vec::new();
    for (i, c) in list["items"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        if c["deleted"].as_bool() == Some(true) || (c["selected"].as_bool() == Some(false) && c["primary"].as_bool() != Some(true)) {
            continue;
        }
        let cal_id = c["id"].as_str().unwrap_or_default();
        let remote = format!("{email}|{cal_id}");
        kept.push(remote.clone());
        let color = c["backgroundColor"].as_str().filter(|h| h.len() == 7).unwrap_or(["#0a84ff", "#30d158", "#ff9f0a", "#bf5af2"][i % 4]).to_string();
        let local: String = storage::with_db(|db| {
            db.execute(
                "INSERT INTO calendars(id,account_id,remote_id,name,color_hex,is_primary,can_edit) VALUES(?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(remote_id) DO UPDATE SET name=excluded.name, color_hex=excluded.color_hex, is_primary=excluded.is_primary, can_edit=excluded.can_edit",
                params![
                    uuid::Uuid::new_v4().to_string(), account_id, remote,
                    c["summaryOverride"].as_str().or(c["summary"].as_str()).unwrap_or("Calendar"), color,
                    c["primary"].as_bool().unwrap_or(false), matches!(c["accessRole"].as_str(), Some("owner" | "writer"))
                ],
            )?;
            Ok(db.query_row("SELECT id FROM calendars WHERE remote_id=?1", [&remote], |r| r.get(0))?)
        })?;
        total += sync_window(&email, &local, cal_id).await?;
    }
    // Calendars that disappeared (unsubscribed) leave with their events.
    storage::with_db(|db| {
        let mut st = db.prepare("SELECT id, remote_id FROM calendars WHERE account_id=?1")?;
        let all: Vec<(String, String)> = st.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?.filter_map(|r| r.ok()).collect();
        for (id, rid) in all {
            if rid.contains('|') && !kept.contains(&rid) {
                db.execute("DELETE FROM calendars WHERE id=?1", [id])?;
            }
        }
        Ok(())
    })?;
    Ok(total)
}

async fn sync_window(email: &str, local_cal: &str, cal_id: &str) -> Result<usize> {
    let start = (chrono::Utc::now() - chrono::Duration::days(PAST_DAYS)).format("%Y-%m-%dT00:00:00Z").to_string();
    let end = (chrono::Utc::now() + chrono::Duration::days(FUTURE_DAYS)).format("%Y-%m-%dT00:00:00Z").to_string();
    let mut seen: HashSet<String> = HashSet::new();
    let mut token: Option<String> = None;
    loop {
        let mut qs = vec![("timeMin", start.as_str()), ("timeMax", end.as_str()), ("singleEvents", "true"), ("showDeleted", "false"), ("maxResults", "250")];
        if let Some(t) = &token {
            qs.push(("pageToken", t.as_str()));
        }
        let page = get_ok(&url(&cal(), &["calendars", cal_id, "events"], &qs)).await?;
        let rows: Vec<EventRow> = page["items"].as_array().into_iter().flatten().filter_map(|e| event_row(email, cal_id, e)).collect();
        storage::with_db(|db| {
            let tx = db.unchecked_transaction()?;
            for r in &rows {
                tx.execute(
                    "INSERT INTO calendar_events(id,calendar_id,remote_id,ical_uid,series_master_id,event_type,title,description_html,location,
                        online_meeting_url,start_utc,end_utc,original_timezone,is_all_day,organizer_json,attendees_json,my_response_status,change_key)
                     VALUES(?1,?2,?3,?4,?5,'singleInstance',?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
                     ON CONFLICT(remote_id) DO UPDATE SET title=excluded.title, description_html=excluded.description_html, location=excluded.location,
                        online_meeting_url=excluded.online_meeting_url, start_utc=excluded.start_utc, end_utc=excluded.end_utc, is_all_day=excluded.is_all_day,
                        organizer_json=excluded.organizer_json, attendees_json=excluded.attendees_json, my_response_status=excluded.my_response_status,
                        change_key=excluded.change_key, calendar_id=excluded.calendar_id
                     WHERE calendar_events.sync_status='SYNCED'",
                    params![
                        uuid::Uuid::new_v4().to_string(), local_cal, r.remote_id, r.ical_uid, r.series, r.title, r.description, r.location, r.meeting_url,
                        r.start, r.end, r.tz, r.all_day, r.organizer_json, r.attendees_json, r.my_response, r.change_key
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })?;
        seen.extend(rows.into_iter().map(|r| r.remote_id));
        token = page["nextPageToken"].as_str().map(String::from);
        if token.is_none() {
            break;
        }
    }
    storage::with_db(|db| {
        let mut st = db.prepare("SELECT id, remote_id FROM calendar_events WHERE calendar_id=?1 AND sync_status='SYNCED' AND remote_id IS NOT NULL")?;
        let stale: Vec<String> = st
            .query_map([local_cal], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
            .filter(|(_, rid)| !seen.contains(rid))
            .map(|(id, _)| id)
            .collect();
        for id in &stale {
            db.execute("DELETE FROM calendar_events WHERE id=?1", [id])?;
        }
        Ok(())
    })?;
    Ok(seen.len())
}

async fn rsvp(remote: &str, response: &str) -> Result<(u16, Value)> {
    let (cal_id, ev_id) = split_event(remote)?;
    let status = match response {
        "accept" => "accepted",
        "tentativelyAccept" => "tentative",
        _ => "declined",
    };
    let u = url(&cal(), &["calendars", &cal_id, "events", &ev_id], &[]);
    let (s, ev) = call(Method::GET, &u, None).await?;
    if !(200..300).contains(&s) {
        return Ok((s, ev));
    }
    let mut attendees = ev["attendees"].as_array().cloned().unwrap_or_default();
    for a in attendees.iter_mut().filter(|a| a["self"].as_bool() == Some(true)) {
        a["responseStatus"] = json!(status);
    }
    call(Method::PATCH, &url(&cal(), &["calendars", &cal_id, "events", &ev_id], &[("sendUpdates", "all")]), Some(json!({ "attendees": attendees }))).await
}

async fn create_event(p: &Value) -> Result<(u16, Value)> {
    let remote_cal = p["calendar"].as_str().unwrap_or_default();
    let cal_id = cal_part(remote_cal).to_string();
    let email = remote_cal.split('|').next().unwrap_or_default().to_string();
    let q = if p["event"]["conferenceData"].is_null() { vec![("sendUpdates", "all")] } else { vec![("sendUpdates", "all"), ("conferenceDataVersion", "1")] };
    let (s, v) = call(Method::POST, &url(&cal(), &["calendars", &cal_id, "events"], &q), Some(p["event"].clone())).await?;
    if !(200..300).contains(&s) {
        return Ok((s, v));
    }
    // Answer in the shape the queue expects: our composite id and the meeting link.
    Ok((s, json!({ "id": format!("{email}|{cal_id}|{}", v["id"].as_str().unwrap_or_default()), "onlineMeeting": { "joinUrl": v["hangoutLink"] } })))
}

/// The Google form of a new event (the app's NewEvent is provider-neutral).
pub fn new_event_payload(title: &str, start: i64, end: i64, all_day: bool, location: Option<&str>, body: Option<&str>, attendees: &[String], meet: bool) -> Value {
    let day = |t: i64| chrono::DateTime::from_timestamp(t, 0).unwrap_or_default().format("%Y-%m-%d").to_string();
    let stamp = |t: i64| chrono::DateTime::from_timestamp(t, 0).unwrap_or_default().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut ev = json!({
        "summary": title,
        "start": if all_day { json!({ "date": day(start) }) } else { json!({ "dateTime": stamp(start), "timeZone": "UTC" }) },
        "end": if all_day { json!({ "date": day(end) }) } else { json!({ "dateTime": stamp(end), "timeZone": "UTC" }) },
        "attendees": attendees.iter().map(|a| json!({ "email": a })).collect::<Vec<_>>(),
    });
    if let Some(l) = location.filter(|l| !l.is_empty()) {
        ev["location"] = json!(l);
    }
    if let Some(b) = body.filter(|b| !b.is_empty()) {
        ev["description"] = json!(b);
    }
    if meet {
        ev["conferenceData"] = json!({ "createRequest": { "requestId": uuid::Uuid::new_v4().to_string(), "conferenceSolutionKey": { "type": "hangoutsMeet" } } });
    }
    ev
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub(crate) type Log = Arc<Mutex<Vec<(String, String, Value)>>>;

    /// A pretend Google: `route(method, path+query, body)` answers; every request is recorded.
    pub(crate) async fn fake_google(route: impl Fn(&str, &str, &Value) -> (u16, Value) + Send + Sync + 'static) -> (String, Log) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let log: Log = Arc::new(Mutex::new(vec![]));
        let log2 = log.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    data.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&data).to_string();
                    if let Some(h) = text.find("\r\n\r\n") {
                        let want = text[..h].to_ascii_lowercase().split("content-length:").nth(1).and_then(|r| r.lines().next()?.trim().parse::<usize>().ok()).unwrap_or(0);
                        if data.len() >= h + 4 + want {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&data).to_string();
                let mut first = text.lines().next().unwrap_or("").split_whitespace();
                let (method, path) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
                let body: Value = text.split("\r\n\r\n").nth(1).and_then(|b| serde_json::from_str(b).ok()).unwrap_or(Value::Null);
                log2.lock().unwrap().push((method.clone(), path.clone(), body.clone()));
                let (st, v) = route(&method, &path, &body);
                let out = v.to_string();
                let resp = format!("HTTP/1.1 {st} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len());
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        (base, log)
    }

    pub(crate) fn unb64_for_test(s: &str) -> Vec<u8> {
        unb64(s).unwrap()
    }

    pub(crate) fn raw_mail(subject: &str, html: &str, inline: Option<(&str, &[u8])>, attach: bool) -> String {
        let mut b = mail_builder::MessageBuilder::new()
            .from(("Mum", "mum@example.com"))
            .to(("Me", "g@x.com"))
            .cc("dad@example.com")
            .subject(subject)
            .message_id("orig-1@example.com")
            .html_body(html);
        if let Some((cid, bytes)) = inline {
            b = b.inline("image/png", cid, bytes);
        }
        if attach {
            b = b.attachment("application/pdf", "bill.pdf", b"%PDF-1.4 fake".as_ref());
        }
        auth::b64(&b.write_to_vec().unwrap())
    }

    fn msg_json(id: &str, labels: &[&str], raw: &str, ms: i64) -> Value {
        json!({ "id": id, "threadId": format!("t-{id}"), "labelIds": labels, "snippet": "snip", "internalDate": ms.to_string(), "historyId": "42", "raw": raw })
    }

    #[test]
    fn parses_encoded_mail_with_inline_picture_and_attachment() {
        let raw = auth::b64(
            &mail_builder::MessageBuilder::new()
                .from(("Zoë Müller", "zoe@example.com"))
                .to("me@x.com")
                .subject("Grüße – Büro ✓")
                .html_body("<p>Hi <img src=\"cid:pic1\"></p>")
                .inline("image/png", "pic1", [1u8, 2, 3].as_ref())
                .attachment("application/pdf", "bill.pdf", [9u8; 20].as_ref())
                .write_to_vec()
                .unwrap(),
        );
        let p = parse_raw(&unb64(&raw).unwrap()).unwrap();
        assert_eq!(p.subject.as_deref(), Some("Grüße – Büro ✓"));
        assert_eq!(p.from_name.as_deref(), Some("Zoë Müller"));
        assert_eq!(p.from_addr.as_deref(), Some("zoe@example.com"));
        assert!(p.html.contains("<p>Hi"));
        assert_eq!(p.inline.len(), 1);
        assert_eq!((p.inline[0].cid.as_str(), p.inline[0].bytes.as_slice()), ("pic1", [1u8, 2, 3].as_slice()));
        assert_eq!(p.files.len(), 1);
        assert_eq!(p.files[0].0, "bill.pdf");
        // Plain-text-only mail still gets an HTML body.
        let txt = parse_raw(b"From: a@b.c\r\nTo: d@e.f\r\nSubject: plain\r\n\r\nhello & goodbye\r\n").unwrap();
        assert!(txt.html.contains("hello"));
    }

    #[test]
    fn blind_copies_are_a_bcc_header_for_the_gmail_api_and_nothing_else() {
        // Without blind copies there is no Bcc header at all.
        let raw = build_raw(("Me", "g@x.com"), &["a@b.c".into()], &["c@d.e".into()], &[], "Hi", "<p>x</p>", None, &[]).unwrap();
        let text = String::from_utf8_lossy(&raw).to_string();
        // (Only header lines count: the random MIME boundary can contain the letters "bcc".)
        assert!(!text.lines().any(|l| l.to_ascii_lowercase().starts_with("bcc:")), "{text}");
        assert!(!text.contains("hidden"));
        // With them, the Gmail API needs the Bcc header (it turns it into envelope recipients and strips it on delivery);
        // the address must not be in To or Cc.
        let raw = build_raw(("Me", "g@x.com"), &["a@b.c".into()], &["c@d.e".into()], &["hidden@x.com".into()], "Hi", "<p>x</p>", None, &[]).unwrap();
        let text = String::from_utf8_lossy(&raw).to_string();
        let header = |name: &str| text.lines().find(|l| l.starts_with(name)).unwrap_or("").to_string();
        assert_eq!(header("Bcc:"), "Bcc: <hidden@x.com>");
        assert!(!header("To:").contains("hidden") && !header("Cc:").contains("hidden"), "{text}");
        assert_eq!(text.matches("hidden@x.com").count(), 1);
    }

    #[test]
    fn builds_replies_that_thread() {
        let raw = build_raw(("Me", "g@x.com"), &["a@b.c".into()], &["c@d.e".into()], &[], "Re: Plans", "<p>Yes</p>", Some(("<orig-1@example.com>", "")), &[]).unwrap();
        let text = String::from_utf8_lossy(&raw).to_string();
        assert!(text.contains("In-Reply-To: <orig-1@example.com>"), "{text}");
        assert!(text.contains("References: <orig-1@example.com>"));
        let back = parse_raw(&raw).unwrap();
        assert_eq!(back.subject.as_deref(), Some("Re: Plans"));
        assert!(back.html.contains("Yes"));
        assert!(back.to_json.contains("a@b.c") && back.cc_json.contains("c@d.e"));
    }

    #[test]
    fn maps_calendar_events() {
        let timed = json!({ "id": "e1", "summary": "Standup", "etag": "x", "start": { "dateTime": "2026-10-20T09:00:00+02:00", "timeZone": "Europe/Paris" },
            "end": { "dateTime": "2026-10-20T09:30:00+02:00" }, "hangoutLink": "https://meet.google.com/abc",
            "organizer": { "email": "boss@x.com", "displayName": "Boss" },
            "attendees": [ { "email": "g@x.com", "self": true, "responseStatus": "tentative" }, { "email": "boss@x.com", "responseStatus": "accepted" } ] });
        let r = event_row("g@x.com", "g@x.com", &timed).unwrap();
        assert_eq!(r.start, 1792479600 - 3600 * 2 + 3600 * 2 - 0 - 0); // 07:00 UTC
        assert_eq!(r.my_response, "tentativelyAccepted");
        assert_eq!(r.meeting_url.as_deref(), Some("https://meet.google.com/abc"));
        assert_eq!(r.remote_id, "g@x.com|g@x.com|e1");
        let all_day = json!({ "id": "e2", "summary": "Holiday", "start": { "date": "2026-12-25" }, "end": { "date": "2026-12-26" }, "creator": { "self": true } });
        let d = event_row("g@x.com", "c", &all_day).unwrap();
        assert!(d.all_day && d.end - d.start == 86400);
        assert_eq!(d.my_response, "organizer");
        assert!(event_row("g@x.com", "c", &json!({ "id": "e3", "status": "cancelled", "start": { "date": "2026-12-25" }, "end": { "date": "2026-12-26" } })).is_none());
    }

    #[tokio::test]
    async fn gmail_end_to_end() {
        use std::sync::atomic::{AtomicU8, Ordering::SeqCst};
        let _guard = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("gm-gmail-{}", uuid::Uuid::new_v4()));
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.join("cfg"));
            std::env::set_var("GM_TEST_TOKEN", "t");
        }
        storage::initialize(tmp.join("vault").to_str().unwrap()).unwrap();
        storage::with_db(|db| {
            db.execute("INSERT INTO accounts(id,email,display_name,created_at,provider) VALUES('g1','g@x.com','Gee',1,'google')", [])?;
            Ok(())
        })
        .unwrap();

        let phase = Arc::new(AtomicU8::new(0));
        let ph = phase.clone();
        let tomorrow = chrono::Utc::now() + chrono::Duration::days(1);
        let (t0, t1) = (tomorrow.format("%Y-%m-%dT10:00:00Z").to_string(), tomorrow.format("%Y-%m-%dT11:00:00Z").to_string());
        let png: &[u8] = &[137, 80, 78, 71, 1, 2, 3];
        let m1 = raw_mail("Budget review", "<p>Hello, the budget is attached <img src=\"cid:logo1\"></p>", Some(("logo1", png)), true);
        let m2 = raw_mail("Trip plans", "<p>Family trip</p>", None, false);
        let m3 = raw_mail("Brand new", "<p>Fresh mail</p>", None, false);
        let event = json!({ "id": "e1", "summary": "Standup", "etag": "\"1\"", "start": { "dateTime": t0 }, "end": { "dateTime": t1 },
            "organizer": { "email": "boss@x.com", "displayName": "Boss" },
            "attendees": [ { "email": "g@x.com", "self": true, "responseStatus": "needsAction" }, { "email": "boss@x.com", "responseStatus": "accepted" } ] });
        let ev2 = event.clone();
        let (base, log) = fake_google(move |m, path, body| {
            let p = path.split('?').next().unwrap_or("");
            let g = "/gmail/v1/users/me";
            let phase = ph.load(SeqCst);
            match (m, p) {
                ("GET", x) if x == format!("{g}/profile") => (200, json!({ "emailAddress": "g@x.com", "historyId": if phase == 2 { "700" } else { "500" } })),
                ("GET", x) if x == format!("{g}/labels") => (200, json!({ "labels": [
                    { "id": "INBOX", "name": "INBOX", "type": "system" }, { "id": "SENT", "name": "SENT", "type": "system" },
                    { "id": "TRASH", "name": "TRASH", "type": "system" }, { "id": "UNREAD", "name": "UNREAD", "type": "system" },
                    { "id": "CATEGORY_PROMOTIONS", "name": "CATEGORY_PROMOTIONS", "type": "system" },
                    { "id": "Label_1", "name": "Family", "type": "user" }, { "id": "Label_2", "name": "Family/Trips", "type": "user" } ] })),
                ("GET", x) if x.starts_with(&format!("{g}/labels/")) => (200, json!({ "id": x.rsplit('/').next(), "messagesUnread": 1, "messagesTotal": 2 })),
                ("GET", x) if x == format!("{g}/messages") => (200, json!({ "messages": if phase == 2 { vec![json!({ "id": "m1" }), json!({ "id": "m3" })] } else { vec![json!({ "id": "m1" }), json!({ "id": "m2" })] } })),
                ("GET", x) if x == format!("{g}/messages/m1") && path.contains("minimal") => (200, json!({ "id": "m1", "threadId": "t-m1", "labelIds": ["INBOX"], "snippet": "snip", "internalDate": "1760000000000", "historyId": "600" })),
                ("GET", x) if x == format!("{g}/messages/m1") => (200, msg_json("m1", &["INBOX", "UNREAD"], &m1, 1760000000000)),
                ("GET", x) if x == format!("{g}/messages/m2") => (200, msg_json("m2", &["INBOX", "Label_2"], &m2, 1760000100000)),
                ("GET", x) if x == format!("{g}/messages/m3") => (200, msg_json("m3", &["INBOX", "UNREAD"], &m3, 1760000200000)),
                ("GET", x) if x == format!("{g}/history") => match phase {
                    1 => (200, json!({ "historyId": "600", "history": [ { "messagesAdded": [ { "message": { "id": "m3" } } ],
                        "labelsRemoved": [ { "message": { "id": "m1" }, "labelIds": ["UNREAD"] } ], "messagesDeleted": [ { "message": { "id": "m2" } } ] } ] })),
                    _ => (404, json!({ "error": { "message": "Requested entity was not found." } })),
                },
                ("POST", x) if x.ends_with("/modify") || x.ends_with("/trash") || x.ends_with("/untrash") => (200, json!({ "id": x.rsplit('/').nth(1) })),
                ("POST", x) if x == format!("{g}/messages/send") => (200, json!({ "id": "sent1" })),
                ("POST", x) if x == format!("{g}/labels") => (200, json!({ "id": "Label_3", "name": body["name"] })),
                ("PATCH", x) if x.contains("/labels/") => (200, json!({})),
                ("DELETE", x) if x.contains("/labels/") => (204, Value::Null),
                ("GET", "/calendar/v3/users/me/calendarList") => (200, json!({ "items": [
                    { "id": "g@x.com", "summary": "Gee", "primary": true, "accessRole": "owner", "backgroundColor": "#9fe1e7" },
                    { "id": "holidays", "summary": "Holidays", "accessRole": "reader", "selected": false } ] })),
                ("GET", x) if x.ends_with("/events") => (200, json!({ "items": [ ev2.clone() ] })),
                ("GET", x) if x.ends_with("/events/e1") => (200, ev2.clone()),
                ("PATCH", x) if x.ends_with("/events/e1") => (200, json!({})),
                ("POST", x) if x.ends_with("/events") => (200, json!({ "id": "newev", "hangoutLink": "https://meet.google.com/new" })),
                ("DELETE", x) if x.contains("/events/") => (204, Value::Null),
                _ => (404, json!({ "error": { "message": format!("no route {m} {path}") } })),
            }
        })
        .await;
        unsafe { std::env::set_var("GM_TEST_GOOGLE", &base) };
        let local = |remote: &str| -> String { storage::with_db(|db| Ok(db.query_row("SELECT id FROM messages WHERE remote_id=?1", [remote], |r| r.get(0))?)).unwrap() };
        let folder = |remote: &str| -> String { storage::with_db(|db| Ok(db.query_row("SELECT id FROM folders WHERE account_id='g1' AND remote_id=?1", [remote], |r| r.get(0))?)).unwrap() };
        let ids = |folder_id: &str| -> Vec<String> {
            crate::mail::messages(folder_id, 0, 50).unwrap().into_iter().map(|m| m.subject.unwrap_or_default()).collect()
        };

        // ---- first sync
        let rep = sync_account("g1").await.unwrap();
        assert_eq!(rep.messages_changed, 2);
        assert_eq!(rep.calendar_error, None);
        let names: HashSet<String> = storage::with_db(|db| {
            let mut st = db.prepare("SELECT display_name FROM folders WHERE account_id='g1'")?;
            Ok(st.query_map([], |r| r.get(0))?.filter_map(|r| r.ok()).collect())
        })
        .unwrap();
        assert_eq!(names, ["Inbox", "Sent", "Trash", "Family", "Trips", "Archive"].iter().map(|s| s.to_string()).collect());
        let (trips_parent, inbox_wk): (Option<String>, Option<String>) = storage::with_db(|db| {
            Ok((
                db.query_row("SELECT parent_remote_id FROM folders WHERE remote_id='Label_2'", [], |r| r.get(0))?,
                db.query_row("SELECT well_known_name FROM folders WHERE remote_id='INBOX'", [], |r| r.get(0))?,
            ))
        })
        .unwrap();
        assert_eq!((trips_parent.as_deref(), inbox_wk.as_deref()), (Some("Label_1"), Some("inbox")));
        assert_eq!(ids(&folder("INBOX")), vec!["Trip plans", "Budget review"]); // newest first
        assert_eq!(ids(&folder("Label_2")), vec!["Trip plans"]); // a message shows under each of its labels
        let detail = crate::mail::message(&local("m1")).unwrap();
        assert!(detail.body_html.as_deref().unwrap().contains("data:image/png;base64,"), "inline picture embedded");
        assert!(!detail.body_html.as_deref().unwrap().contains("cid:logo1"));
        assert_eq!(crate::mail::search("budget", 10).unwrap().len(), 1);
        let (unread, attach): (bool, bool) = storage::with_db(|db| Ok(db.query_row("SELECT is_read=0, has_attachments FROM messages WHERE remote_id='m1'", [], |r| Ok((r.get(0)?, r.get(1)?)))?)).unwrap();
        assert!(unread && attach);
        assert_eq!(state_get("g1", "history_id").as_deref(), Some("500"));
        let evs = crate::calendar::events(0, i64::MAX / 2).unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(crate::calendar::calendars().unwrap().len(), 1, "unselected calendars are skipped");

        // ---- incremental: m3 arrives, m1 gets read elsewhere, m2 deleted
        phase.store(1, SeqCst);
        let rep = sync_account("g1").await.unwrap();
        assert_eq!(rep.messages_changed, 3);
        assert_eq!(ids(&folder("INBOX")), vec!["Brand new", "Budget review"]);
        let read: bool = storage::with_db(|db| Ok(db.query_row("SELECT is_read FROM messages WHERE remote_id='m1'", [], |r| r.get(0))?)).unwrap();
        assert!(read);
        assert_eq!(state_get("g1", "history_id").as_deref(), Some("600"));
        assert!(!log.lock().unwrap().iter().any(|(_, p, _)| p.contains("/messages/m3") && p.contains("minimal")), "new mail is downloaded whole");

        // ---- Gmail forgot our bookmark: start over without losing anything
        phase.store(2, SeqCst);
        sync_account("g1").await.unwrap();
        assert_eq!(state_get("g1", "history_id").as_deref(), Some("700"));
        assert_eq!(ids(&folder("INBOX")).len(), 2);

        // ---- offline actions
        phase.store(3, SeqCst);
        log.lock().unwrap().clear();
        let (m1l, m3l) = (local("m1"), local("m3"));
        crate::outbox::set_read(&m1l, false).unwrap();
        crate::outbox::set_flag(&m1l, true).unwrap();
        crate::outbox::move_to(&m1l, &folder(ARCHIVE)).unwrap();
        crate::outbox::delete(&m3l).unwrap(); // -> Trash
        assert_eq!(ids(&folder("INBOX")), Vec::<String>::new(), "moved mail leaves the Inbox view at once");
        assert_eq!(crate::outbox::replay().await.unwrap(), 4 - 0);
        let calls: Vec<(String, String, Value)> = log.lock().unwrap().clone();
        let find = |suffix: &str| calls.iter().find(|(_, p, _)| p.split('?').next().unwrap().ends_with(suffix)).cloned().unwrap_or_else(|| panic!("no call to {suffix}: {calls:?}"));
        assert_eq!(find("/messages/m1/modify").2["addLabelIds"], json!(["UNREAD"]));
        assert!(calls.iter().any(|(_, p, b)| p.ends_with("/messages/m1/modify") && b["addLabelIds"] == json!(["STARRED"])));
        assert!(calls.iter().any(|(_, p, b)| p.ends_with("/messages/m1/modify") && b["removeLabelIds"] == json!(["INBOX", "SPAM"])), "archive = leave the Inbox");
        find("/messages/m3/trash");
        assert_eq!(crate::outbox::pending_count().unwrap(), 0);

        // out of the Trash into a label: untrash first, then file it
        log.lock().unwrap().clear();
        crate::outbox::move_to(&m3l, &folder("Label_1")).unwrap();
        crate::outbox::replay().await.unwrap();
        let calls: Vec<(String, String, Value)> = log.lock().unwrap().clone();
        let order: Vec<String> = calls.iter().map(|(_, p, _)| p.rsplit('/').next().unwrap().to_string()).collect();
        assert_eq!(order, vec!["untrash", "modify"]);
        assert_eq!(calls[1].2["addLabelIds"], json!(["Label_1"]));

        // copy = one more label (shown under it at once); spam = the SPAM label
        log.lock().unwrap().clear();
        crate::outbox::copy_to(&m1l, &folder("Label_2")).unwrap();
        assert!(ids(&folder("Label_2")).contains(&"Budget review".to_string()), "a copy shows under its label at once");
        storage::with_db(|db| Ok(db.execute("INSERT OR IGNORE INTO folders(id,account_id,remote_id,display_name,well_known_name) VALUES('g1-spam','g1','SPAM','Spam','junkemail')", [])?)).unwrap();
        crate::outbox::mark_spam(&m1l).unwrap();
        crate::outbox::replay().await.unwrap();
        let calls: Vec<(String, String, Value)> = log.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!((calls[0].2["addLabelIds"].clone(), calls[0].2["removeLabelIds"].clone()), (json!(["Label_2"]), json!([])));
        assert_eq!(calls[1].2["addLabelIds"], json!(["SPAM"]));
        assert_eq!(crate::outbox::sender_of(&m1l).unwrap().0, "g@x.com");

        // replying joins the conversation and comes from the receiving account
        log.lock().unwrap().clear();
        crate::outbox::send(crate::outbox::Draft { mode: "reply".into(), reply_to: Some(m1l.clone()), to: vec!["mum@example.com".into()], cc: vec![], bcc: vec![], subject: "Re: Budget review".into(), html: "<p>Thanks</p>".into(), from: None, attachments: vec![] }).unwrap();
        crate::outbox::send(crate::outbox::Draft { mode: "new".into(), reply_to: None, to: vec!["z@z.com".into()], cc: vec![], bcc: vec!["hidden@z.com".into()], subject: "Hello".into(), html: "<p>New</p>".into(), from: Some("g@x.com".into()), attachments: vec![] }).unwrap();
        crate::outbox::replay().await.unwrap();
        let sends: Vec<Value> = log.lock().unwrap().iter().filter(|(_, p, _)| p.ends_with("/messages/send")).map(|(_, _, b)| b.clone()).collect();
        assert_eq!(sends.len(), 2);
        assert_eq!(sends[0]["threadId"], "t-m1");
        let raw = String::from_utf8(unb64(sends[0]["raw"].as_str().unwrap()).unwrap()).unwrap();
        assert!(raw.contains("In-Reply-To: <orig-1@example.com>") && raw.contains("Subject: Re: Budget review") && raw.contains("Gee"), "{raw}");
        assert!(!raw.to_ascii_lowercase().contains("bcc"), "a reply without blind copies has no Bcc header: {raw}");
        assert!(sends[1]["threadId"].is_null());
        // The blind copy goes to the Gmail API as a Bcc header only (Gmail strips it on delivery), never in To/Cc.
        let raw = String::from_utf8(unb64(sends[1]["raw"].as_str().unwrap()).unwrap()).unwrap();
        let header = |name: &str| raw.lines().find(|l| l.starts_with(name)).unwrap_or("").to_string();
        assert_eq!((header("To:"), header("Bcc:")), ("To: <z@z.com>".to_string(), "Bcc: <hidden@z.com>".to_string()), "{raw}");
        assert!(!header("Cc:").contains("hidden") && raw.matches("hidden@z.com").count() == 1, "{raw}");

        // ---- calendar actions
        log.lock().unwrap().clear();
        let ev_local: String = storage::with_db(|db| Ok(db.query_row("SELECT id FROM calendar_events", [], |r| r.get(0))?)).unwrap();
        crate::calendar::rsvp(&ev_local, "accept").unwrap();
        let cal_local: String = storage::with_db(|db| Ok(db.query_row("SELECT id FROM calendars", [], |r| r.get(0))?)).unwrap();
        let new_id = crate::calendar::create(crate::calendar::NewEvent {
            calendar_id: cal_local,
            title: "Lunch".into(), start_utc: 1_900_000_000, end_utc: 1_900_003_600, is_all_day: false, location: Some("Cafe".into()),
            attendees: vec!["a@b.c".into()], body: None, online_meeting: true,
        })
        .unwrap();
        crate::outbox::replay().await.unwrap();
        let calls: Vec<(String, String, Value)> = log.lock().unwrap().clone();
        let patch = calls.iter().find(|(m, _, _)| m == "PATCH").expect("RSVP patch");
        assert!(patch.1.contains("sendUpdates=all"));
        assert_eq!(patch.2["attendees"][0]["responseStatus"], "accepted");
        let create = calls.iter().find(|(m, p, _)| m == "POST" && p.contains("/events")).expect("create");
        assert!(create.1.contains("conferenceDataVersion=1"));
        assert_eq!(create.2["summary"], "Lunch");
        assert_eq!(create.2["conferenceData"]["createRequest"]["conferenceSolutionKey"]["type"], "hangoutsMeet");
        let (rid, url_): (String, Option<String>) = storage::with_db(|db| Ok(db.query_row("SELECT remote_id, online_meeting_url FROM calendar_events WHERE id=?1", [&new_id], |r| Ok((r.get(0)?, r.get(1)?)))?)).unwrap();
        assert_eq!((rid.as_str(), url_.as_deref()), ("g@x.com|g@x.com|newev", Some("https://meet.google.com/new")));
        log.lock().unwrap().clear();
        crate::calendar::delete(&new_id).unwrap();
        crate::outbox::replay().await.unwrap();
        assert!(log.lock().unwrap().iter().any(|(m, p, _)| m == "DELETE" && p.contains("/events/newev") && p.contains("sendUpdates=all")));
        assert_eq!(crate::outbox::pending_count().unwrap(), 0);
        assert!(crate::outbox::overview().unwrap().is_empty(), "nothing failed");

        // ---- labels as folders
        log.lock().unwrap().clear();
        let family = folder("Label_1");
        let sub = create_label("Sub", Some(&family), "g1").await.unwrap();
        assert_eq!(log.lock().unwrap().last().unwrap().2["name"], "Family/Sub");
        assert_eq!(crate::mail::folders().unwrap().iter().filter(|f| f.account_email == "g@x.com").count(), 8); // incl. the Spam folder added above
        log.lock().unwrap().clear();
        rename_label(&family, "Kin").await.unwrap();
        let patches: Vec<(String, Value)> = log.lock().unwrap().iter().filter(|(m, _, _)| m == "PATCH").map(|(_, p, b)| (p.clone(), b["name"].as_str().unwrap().to_string().into())).collect();
        let named: HashSet<String> = patches.iter().map(|(_, n)| n.as_str().unwrap().to_string()).collect();
        assert_eq!(named, ["Kin", "Kin/Trips", "Kin/Sub"].iter().map(|s| s.to_string()).collect());
        assert!(rename_label(&folder("INBOX"), "x").await.is_err());
        log.lock().unwrap().clear();
        delete_label(&family).await.unwrap();
        assert_eq!(log.lock().unwrap().iter().filter(|(m, _, _)| m == "DELETE").count(), 3); // Kin, Trips, Sub
        let left: i64 = storage::with_db(|db| Ok(db.query_row("SELECT COUNT(*) FROM folders WHERE account_id='g1' AND remote_id IN ('Label_1','Label_2','Label_3')", [], |r| r.get(0))?)).unwrap();
        assert_eq!(left, 0);
        let _ = sub;
        let orphan_folder: String = storage::with_db(|db| Ok(db.query_row("SELECT f.display_name FROM messages m JOIN folders f ON f.id=m.folder_id WHERE m.remote_id='m3'", [], |r| r.get(0))?)).unwrap();
        assert_eq!(orphan_folder, "Archive", "mail from a deleted label stays, in Archive");
    }

    #[test]
    fn google_client_settings_come_from_outside_the_source() {
        // run-time values win; both id and secret must exist
        assert_eq!(resolve_client(None, Some("S".into()), "ID", "").unwrap(), ("ID".into(), "S".into()));
        assert_eq!(resolve_client(Some("E".into()), None, "ID", "built").unwrap(), ("E".into(), "built".into()));
        assert!(resolve_client(None, None, "ID", "").unwrap_err().to_string().contains("secret is missing"));
        assert!(resolve_client(Some(String::new()), Some(String::new()), "", "").is_err());
    }
}
