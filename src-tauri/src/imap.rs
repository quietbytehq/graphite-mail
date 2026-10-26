//! Gmail through IMAP and SMTP with a Google app password. There is no OAuth here, so this account type needs no
//! Google client (and no client secret): it works in every build. The app password lives only in the OS keyring.
//!
//! Mapping onto the shared tables, like the Gmail API provider: one local message per Gmail message (Gmail's
//! X-GM-MSGID; plain IMAP servers get one per folder and UID), `imap_uids` records every (folder, UID) it is stored
//! under, its primary folder is `messages.folder_id`, and every folder it is in is also listed in `message_labels`.
//! A folder's bookmark ("UIDVALIDITY:UIDNEXT:MESSAGES:HIGHESTMODSEQ") is kept in `folders.delta_link`.

use crate::{auth, google, storage, sync};
use anyhow::{Result, anyhow, bail};
use async_imap::types::{Fetch, Flag, NameAttribute};
use futures::StreamExt;
use mail_parser::{MessageParser, MimeHeaders};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PROVIDER: &str = "imap";

/// Where an "Other email (IMAP)" account's servers are. Absent for Gmail accounts (Gmail's servers are built in).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct ImapSettings {
    /// "yahoo", "icloud", "zoho", "fastmail", "aol" or "custom": only for the label in Settings.
    #[serde(default)]
    pub preset: String,
    pub imap_host: String,
    pub imap_port: u16,
    /// "tls" (implicit, usually 993/465), "starttls" (143/587) or "none" (plain; tests only, never advised).
    pub imap_tls: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: String,
    /// Sign-in name when it isn't the address (most providers: the full address).
    #[serde(default)]
    pub username: String,
}

impl ImapSettings {
    /// Refuses settings that can't work before any connection is tried.
    pub fn check(&self) -> Result<()> {
        for (what, host) in [("IMAP", &self.imap_host), ("SMTP", &self.smtp_host)] {
            let h = host.trim();
            if h.is_empty() || h.contains(char::is_whitespace) || h.contains('/') || h.contains(':') {
                bail!("Enter the {what} server's host name, for example imap.example.com.");
            }
        }
        if self.imap_port == 0 || self.smtp_port == 0 {
            bail!("Ports go from 1 to 65535.");
        }
        for t in [&self.imap_tls, &self.smtp_tls] {
            if !matches!(t.as_str(), "tls" | "starttls" | "none") {
                bail!("The security setting must be TLS, STARTTLS or none.");
            }
        }
        Ok(())
    }
}

const IMAP_HOST: &str = "imap.gmail.com";
const IMAP_PORT: u16 = 993;
const SMTP_HOST: &str = "smtp.gmail.com";
const SMTP_PORT: u16 = 465;

/// First sync of a folder: how far back, and at most how many messages.
const INITIAL_DAYS: i64 = 90;
const INITIAL_MAX: usize = 3000;
/// Bodies downloaded per sync pass, newest first. Bigger messages wait until they are opened.
const BODIES_PER_PASS: usize = 150;
const BACKGROUND_MAX_BYTES: i64 = 5 * 1024 * 1024;
const HEADER_CHUNK: usize = 200;
/// One body download asks for at most this many messages, or this many bytes, so each answer comes back quickly.
const BODY_CHUNK: usize = 20;
const BODY_CHUNK_BYTES: i64 = 2 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const NET_TIMEOUT: Duration = Duration::from_secs(90);
/// A pooled connection idle for longer than this is checked with NOOP before it is used again.
const IDLE_CHECK: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------- the app password

/// An app password. Debug output never shows it, so it can't end up in a log by accident.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Google shows app passwords as four groups ("abcd efgh ijkl mnop"); the spaces are not part of it.
    pub fn new(raw: &str) -> Secret {
        Secret(raw.chars().filter(|c| !c.is_whitespace()).collect())
    }

    fn expose(&self) -> &str {
        &self.0
    }

    /// Takes the password (with or without Google's spaces) out of text that is about to be shown or logged.
    pub fn redact(&self, text: &str) -> String {
        if self.0.is_empty() {
            return text.to_string();
        }
        let grouped = self.0.chars().collect::<Vec<_>>().chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join(" ");
        text.replace(&grouped, "[app password]").replace(&self.0, "[app password]")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

fn password(email: &str) -> Result<Secret> {
    auth::app_password(email)
        .map(|p| Secret::new(&p))
        .map_err(|_| refused(401, format!("The app password for {email} is missing from the keyring. Remove the account in Settings and add it again.")))
}

// ---------------------------------------------------------------- errors

/// The server turned something down (as opposed to being out of reach). `status` follows the offline queue's
/// HTTP-like contract: 400 refused for good, 401 sign-in problem, 404 already gone, 503 try again later.
#[derive(Debug)]
struct Refused {
    status: u16,
    message: String,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refused {}

fn refused(status: u16, message: impl Into<String>) -> anyhow::Error {
    Refused { status, message: message.into() }.into()
}

fn out_of_reach(host: &str, detail: impl fmt::Display) -> anyhow::Error {
    anyhow!("Couldn't reach {host}. Check your internet connection (and that no firewall blocks mail), then try again. ({detail})")
}

/// Turns Gmail's answer to a refused sign-in into advice. `text` must already be redacted.
fn login_problem(email: &str, text: &str, gmail: bool) -> anyhow::Error {
    let t = text.to_ascii_lowercase();
    if !gmail {
        return refused(401, format!(
            "The mail server didn't accept the sign-in for {email}. Check the user name and the app password (most providers want an app \
             password made in your account's security settings, not your normal password), and that IMAP is switched on for the account."
        ));
    }
    let msg = if t.contains("application-specific password") || t.contains("web login required") || t.contains("webalert") {
        format!(
            "Google wants an app password for {email}, not your normal password. App passwords need 2-Step Verification: \
             turn it on for your Google account, then create an app password at myaccount.google.com/apppasswords."
        )
    } else if t.contains("not enabled for imap") || (t.contains("imap") && t.contains("disabled")) {
        format!("IMAP is switched off for {email}. In Gmail open Settings → See all settings → Forwarding and POP/IMAP, enable IMAP, then try again.")
    } else {
        format!(
            "Google didn't accept the app password for {email}. Check the email address, and paste the 16-letter app password again \
             (or create a new one at myaccount.google.com/apppasswords)."
        )
    };
    refused(401, msg)
}

fn smtp_problem(email: &str, pw: &Secret, e: lettre::transport::smtp::Error) -> anyhow::Error {
    smtp_problem_with(email, pw, e, settings_of(email).as_ref())
}

fn smtp_problem_with(email: &str, pw: &Secret, e: lettre::transport::smtp::Error, settings: Option<&ImapSettings>) -> anyhow::Error {
    let text = pw.redact(&e.to_string());
    let code = e.status().map(|c| c.to_string()).unwrap_or_default();
    let gmail = settings.is_none();
    let who = if gmail { "Gmail" } else { "The mail server" };
    if gmail && (text.contains("5.7.9") || text.to_ascii_lowercase().contains("application-specific")) {
        return login_problem(email, "application-specific password required", true);
    }
    if code == "535" || code == "534" || text.contains("5.7.8") {
        return refused(401, if gmail {
            format!("Gmail didn't accept the app password for sending from {email}. Create a new app password at myaccount.google.com/apppasswords and add the account again.")
        } else {
            format!("{who} didn't accept the sign-in for sending from {email}. Check the SMTP user name and the app password, and add the account again.")
        });
    }
    if e.is_permanent() {
        return refused(400, format!("{who} refused to send this message: {text}"));
    }
    if e.is_transient() {
        return refused(503, format!("{who} can't take this message right now; it will be sent later. ({text})"));
    }
    out_of_reach(&smtp_server_with(email, settings).host, text)
}

// ---------------------------------------------------------------- servers

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tls {
    /// TLS from the first byte (IMAP 993, SMTP 465).
    Implicit,
    /// Plain connection upgraded with STARTTLS (IMAP 143, SMTP 587).
    StartTls,
    /// No encryption (pretend servers in tests; a custom server on a trusted network).
    Plain,
}

impl Tls {
    fn parse(s: &str) -> Tls {
        match s {
            "starttls" => Tls::StartTls,
            "none" => Tls::Plain,
            _ => Tls::Implicit,
        }
    }
}

struct Server {
    host: String,
    port: u16,
    tls: Tls,
    /// The sign-in name (the address unless the account says otherwise).
    user: String,
}

/// The servers of an account: its own settings ("Other email"), else Gmail's.
fn settings_of(email: &str) -> Option<ImapSettings> {
    auth::accounts().into_iter().find(|a| a.email.eq_ignore_ascii_case(email)).and_then(|a| a.imap)
}

fn user_of(email: &str, s: Option<&ImapSettings>) -> String {
    s.map(|s| s.username.trim().to_string()).filter(|u| !u.is_empty()).unwrap_or_else(|| email.to_string())
}

/// Servers for an account being added (its settings aren't stored yet) or already stored.
fn imap_server_with(email: &str, s: Option<&ImapSettings>) -> Server {
    if let Some(s) = s {
        return Server { host: s.imap_host.trim().into(), port: s.imap_port, tls: Tls::parse(&s.imap_tls), user: user_of(email, Some(s)) };
    }
    #[cfg(test)]
    if let Some(s) = test_server("GM_TEST_IMAP", email) {
        return s;
    }
    Server { host: IMAP_HOST.into(), port: IMAP_PORT, tls: Tls::Implicit, user: email.into() }
}

fn smtp_server_with(email: &str, s: Option<&ImapSettings>) -> Server {
    if let Some(s) = s {
        return Server { host: s.smtp_host.trim().into(), port: s.smtp_port, tls: Tls::parse(&s.smtp_tls), user: user_of(email, Some(s)) };
    }
    #[cfg(test)]
    if let Some(s) = test_server("GM_TEST_SMTP", email) {
        return s;
    }
    Server { host: SMTP_HOST.into(), port: SMTP_PORT, tls: Tls::Implicit, user: email.into() }
}

fn imap_server(email: &str) -> Server {
    imap_server_with(email, settings_of(email).as_ref())
}

fn smtp_server(email: &str) -> Server {
    smtp_server_with(email, settings_of(email).as_ref())
}

/// Tests talk to pretend Gmail servers on 127.0.0.1 without TLS.
#[cfg(test)]
fn test_server(var: &str, email: &str) -> Option<Server> {
    let a = std::env::var(var).ok()?;
    let (h, p) = a.rsplit_once(':')?;
    Some(Server { host: h.into(), port: p.parse().ok()?, tls: Tls::Plain, user: email.into() })
}

// ---------------------------------------------------------------- IMAP connections

trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + fmt::Debug {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + fmt::Debug> Io for T {}

type Session = async_imap::Session<Box<dyn Io>>;

/// Runs one network step with a time limit; a server that stops answering counts as out of reach.
async fn tm<T, E: Into<anyhow::Error>>(f: impl std::future::Future<Output = std::result::Result<T, E>>) -> Result<T> {
    match tokio::time::timeout(NET_TIMEOUT, f).await {
        Ok(r) => r.map_err(Into::into),
        Err(_) => Err(anyhow!("The mail server stopped answering (timed out).")),
    }
}

fn tls_connector() -> tokio_rustls::TlsConnector {
    use tokio_rustls::rustls;
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("TLS 1.2/1.3 are supported")
        .with_root_certificates(roots)
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(cfg))
}

async fn open_stream(srv: &Server) -> Result<Box<dyn Io>> {
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect((srv.host.as_str(), srv.port)))
        .await
        .map_err(|_| out_of_reach(&srv.host, "timed out"))?
        .map_err(|e| out_of_reach(&srv.host, e))?;
    // Commands are small and each waits for its answer: without this, every command can sit ~40 ms in the kernel.
    let _ = tcp.set_nodelay(true);
    if srv.tls != Tls::Implicit {
        return Ok(Box::new(tcp)); // plain, or upgraded with STARTTLS by the caller
    }
    let name = tokio_rustls::rustls::pki_types::ServerName::try_from(srv.host.clone())?;
    let tls = tokio::time::timeout(CONNECT_TIMEOUT, tls_connector().connect(name, tcp))
        .await
        .map_err(|_| out_of_reach(&srv.host, "secure connection timed out"))?
        .map_err(|e| out_of_reach(&srv.host, e))?;
    Ok(Box::new(tls))
}

/// One signed-in IMAP connection and what the server can do.
struct Conn {
    s: Session,
    gmail: bool,
    can_move: bool,
    uidplus: bool,
    condstore: bool,
    /// The mailbox currently open (so it can be closed before it is deleted).
    selected: Option<String>,
    used: Instant,
}

async fn login(email: &str, pw: &Secret) -> Result<Conn> {
    login_to(email, pw, settings_of(email).as_ref()).await
}

/// Signs in to the IMAP server (`settings` = the account's own servers, None = Gmail).
async fn login_to(email: &str, pw: &Secret, settings: Option<&ImapSettings>) -> Result<Conn> {
    let srv = imap_server_with(email, settings);
    let gmail_account = settings.is_none();
    let mut client = async_imap::Client::new(open_stream(&srv).await?);
    match tokio::time::timeout(CONNECT_TIMEOUT, client.read_response()).await {
        Ok(Ok(Some(_))) => {}
        _ => return Err(out_of_reach(&srv.host, "the server didn't greet us")),
    }
    if srv.tls == Tls::StartTls {
        // Upgrade the plain connection: the server keeps the session, so no second greeting is read.
        tm(client.run_command_and_check_ok("STARTTLS", None)).await.map_err(|e| out_of_reach(&srv.host, format!("STARTTLS refused: {e}")))?;
        let plain = client.into_inner();
        let name = tokio_rustls::rustls::pki_types::ServerName::try_from(srv.host.clone())?;
        let tls = tokio::time::timeout(CONNECT_TIMEOUT, tls_connector().connect(name, plain))
            .await
            .map_err(|_| out_of_reach(&srv.host, "secure connection timed out"))?
            .map_err(|e| out_of_reach(&srv.host, e))?;
        client = async_imap::Client::new(Box::new(tls));
    }
    let mut s = match tokio::time::timeout(NET_TIMEOUT, client.login(&srv.user, pw.expose())).await {
        Err(_) => return Err(out_of_reach(&srv.host, "sign-in timed out")),
        Ok(Ok(s)) => s,
        Ok(Err((async_imap::error::Error::No(t) | async_imap::error::Error::Bad(t), _))) => return Err(login_problem(email, &pw.redact(&t), gmail_account)),
        Ok(Err((e, _))) => return Err(out_of_reach(&srv.host, pw.redact(&e.to_string()))),
    };
    let caps = tm(s.capabilities()).await?;
    Ok(Conn {
        gmail: caps.has_str("X-GM-EXT-1"),
        can_move: caps.has_str("MOVE"),
        uidplus: caps.has_str("UIDPLUS"),
        condstore: caps.has_str("CONDSTORE"),
        s,
        selected: None,
        used: Instant::now(),
    })
}

impl Conn {
    /// Opens a mailbox (read-only unless `write`) and returns its current UIDVALIDITY. Always asks the server:
    /// a remembered value could be stale, and UIDs must never be used against a renumbered mailbox.
    async fn open_box(&mut self, name: &str, write: bool) -> Result<u32> {
        self.selected = None;
        let mb = if write { tm(self.s.select(name)).await? } else { tm(self.s.examine(name)).await? };
        let v = mb.uid_validity.unwrap_or(0);
        self.selected = Some(name.to_string());
        Ok(v)
    }

    async fn uid_fetch(&mut self, set: &str, query: &str) -> Result<Vec<Fetch>> {
        let s = &mut self.s;
        let items = tm(async move {
            let stream = s.uid_fetch(set, query).await?;
            stream.collect::<Vec<_>>().await.into_iter().collect::<async_imap::error::Result<Vec<Fetch>>>()
        })
        .await?;
        self.drain();
        Ok(items)
    }

    async fn uid_store(&mut self, set: &str, query: &str) -> Result<()> {
        let s = &mut self.s;
        tm(async move {
            let stream = s.uid_store(set, query).await?;
            stream.collect::<Vec<_>>().await.into_iter().collect::<async_imap::error::Result<Vec<Fetch>>>()
        })
        .await?;
        self.drain();
        Ok(())
    }

    async fn uid_search(&mut self, query: &str) -> Result<Vec<u32>> {
        let found = tm(self.s.uid_search(query)).await?;
        self.drain();
        Ok(found.into_iter().collect())
    }

    /// Permanently removes messages already marked \Deleted (only `uid` when the server can do that).
    async fn expunge(&mut self, uid: u32) -> Result<()> {
        let s = &mut self.s;
        let only_this = self.uidplus;
        tm(async move {
            if only_this {
                let stream = s.uid_expunge(uid.to_string()).await?;
                stream.collect::<Vec<_>>().await.into_iter().collect::<async_imap::error::Result<Vec<_>>>()
            } else {
                let stream = s.expunge().await?;
                stream.collect::<Vec<_>>().await.into_iter().collect::<async_imap::error::Result<Vec<_>>>()
            }
        })
        .await?;
        self.drain();
        Ok(())
    }

    /// Changes the server announces on its own (new mail, flag changes) are picked up by the next sync instead.
    fn drain(&mut self) {
        while self.s.unsolicited_responses.try_recv().is_ok() {}
    }
}

/// Two connections per account at most: one for syncing and queued changes, one for what the user is waiting for
/// (opening a message, an attachment), so reading never waits for a long sync.
#[derive(Clone, Copy, PartialEq)]
enum Use {
    Background,
    Reading,
}

static POOL: Mutex<Vec<(String, Use, Conn)>> = Mutex::new(Vec::new());

async fn take(email: &str, purpose: Use) -> Result<Conn> {
    let pooled = {
        let mut p = POOL.lock().unwrap();
        p.iter().position(|(e, u, _)| e.eq_ignore_ascii_case(email) && *u == purpose).map(|i| p.swap_remove(i).2)
    };
    if let Some(mut c) = pooled {
        if c.used.elapsed() < IDLE_CHECK || tm(c.s.noop()).await.is_ok() {
            return Ok(c);
        }
    }
    login(email, &password(email)?).await
}

/// Puts a connection back for the next task, unless the step failed in a way that may have broken it.
fn done<T>(email: &str, purpose: Use, mut c: Conn, outcome: &Result<T>) {
    let healthy = match outcome {
        Ok(_) => true,
        Err(e) => e.downcast_ref::<Refused>().is_some() || matches!(e.downcast_ref::<async_imap::error::Error>(), Some(async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_))),
    };
    if !healthy {
        return;
    }
    c.used = Instant::now();
    c.drain();
    let mut p = POOL.lock().unwrap();
    if !p.iter().any(|(e, u, _)| e.eq_ignore_ascii_case(email) && *u == purpose) {
        p.push((email.to_lowercase(), purpose, c));
    }
}

/// Closes an account's connections (when it is removed).
pub fn forget(email: &str) {
    POOL.lock().unwrap().retain(|(e, _, _)| !e.eq_ignore_ascii_case(email));
}

// ---------------------------------------------------------------- adding an account

/// Checks the address and app password against Gmail's IMAP and SMTP servers, then saves the account.
/// Adds a Gmail (`settings` = None) or other IMAP/SMTP account (`settings` = its servers) with an app password.
pub async fn add_account(email: &str, app_password: &str, settings: Option<ImapSettings>) -> Result<auth::Account> {
    let email = email.trim().to_lowercase();
    let gmail = settings.is_none();
    let at = email.find('@').unwrap_or(0);
    if at == 0 || at == email.len() - 1 || email.contains(char::is_whitespace) || !email[at + 1..].contains('.') {
        bail!(if gmail { "Enter your full Gmail address, for example name@gmail.com." } else { "Enter your full email address, for example name@example.com." });
    }
    let pw = Secret::new(app_password);
    if pw.expose().is_empty() {
        bail!(if gmail { "Paste the app password Google showed you." } else { "Enter the app password for this account." });
    }
    if gmail && (pw.expose().chars().count() != 16 || !pw.expose().chars().all(|c| c.is_ascii_alphanumeric())) {
        bail!("A Google app password has 16 letters (Google shows them as four groups of four). Copy it again from myaccount.google.com/apppasswords.");
    }
    if let Some(s) = &settings {
        s.check()?;
    }
    if let Some(other) = auth::accounts().into_iter().find(|a| a.email.eq_ignore_ascii_case(&email)) {
        if other.provider != PROVIDER {
            bail!("{email} is already in Graphite Mail (signed in with Google). Remove it first to use an app password instead.");
        }
    }

    // 1. Reading mail.
    let mut c = login_to(&email, &pw, settings.as_ref()).await?;
    let _ = tm(c.s.logout()).await;
    // 2. Sending mail.
    let smtp = smtp_server_with(&email, settings.as_ref());
    match tokio::time::timeout(NET_TIMEOUT, transport_to(&email, &pw, settings.as_ref())?.test_connection()).await {
        Err(_) => return Err(out_of_reach(&smtp.host, "timed out")),
        Ok(Err(e)) => return Err(smtp_problem_with(&email, &pw, e, settings.as_ref())),
        Ok(Ok(false)) => return Err(out_of_reach(&smtp.host, "the connection closed")),
        Ok(Ok(true)) => {}
    }
    // 3. Saving: the password goes to the keyring only.
    auth::store_app_password(&email, pw.expose())?;
    let acc = auth::Account {
        display_name: email.clone(),
        email: email.clone(),
        provider: PROVIDER.into(),
        rules: true, // rules for this account are kept on this computer: no extra permission to ask for
        contacts: false,
        personal: false,
        imap: settings,
    };
    if let Err(e) = auth::remember_account(&acc) {
        auth::forget_app_password(&email);
        return Err(e);
    }
    forget(&email); // a re-added account starts with fresh connections
    Ok(acc)
}

// ---------------------------------------------------------------- folder names (modified UTF-7, RFC 3501 5.1.3)

pub fn decode_name(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[i..]);
            return out;
        };
        if end == 0 {
            out.push('&');
        } else {
            use base64::Engine;
            let b64 = after[..end].replace(',', "/");
            match base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64.as_bytes()) {
                Ok(bytes) => {
                    let units: Vec<u16> = bytes.chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                    out.push_str(&String::from_utf16_lossy(&units));
                }
                Err(_) => out.push_str(&rest[i..i + 2 + end]),
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

pub fn encode_name(s: &str) -> String {
    use base64::Engine;
    let mut out = String::new();
    let mut pending: Vec<u16> = vec![];
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if !pending.is_empty() {
            let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
            out.push('&');
            out.push_str(&base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes).replace('/', ","));
            out.push('-');
            pending.clear();
        }
    };
    for ch in s.chars() {
        if (' '..='~').contains(&ch) {
            flush(&mut pending, &mut out);
            if ch == '&' { out.push_str("&-") } else { out.push(ch) }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(ch.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

// ---------------------------------------------------------------- folders

struct Mailbox {
    /// The server's name (encoded), e.g. "[Gmail]/Sent Mail"; stored as the folder's remote id.
    name: String,
    display: String,
    parent: Option<String>,
    well_known: Option<&'static str>,
}

/// Special-use attributes (RFC 6154) → the app's well-known folder names.
fn special_use(name: &str, attrs: &[NameAttribute]) -> Option<&'static str> {
    if name.eq_ignore_ascii_case("INBOX") {
        return Some("inbox");
    }
    attrs.iter().find_map(|a| match a {
        NameAttribute::Sent => Some("sentitems"),
        NameAttribute::Drafts => Some("drafts"),
        NameAttribute::Trash => Some("deleteditems"),
        NameAttribute::Junk => Some("junkemail"),
        NameAttribute::All | NameAttribute::Archive => Some("archive"),
        NameAttribute::Flagged => Some("flagged"),
        NameAttribute::Extension(x) if x.eq_ignore_ascii_case("\\Important") => Some("important"),
        _ => None,
    })
}

/// Non-Gmail servers that don't say what a folder is for (no SPECIAL-USE): the usual names, in a few spellings.
/// Never used for Gmail (its folders all carry attributes) and never for a folder that has an attribute.
pub(crate) fn role_by_name(leaf: &str) -> Option<&'static str> {
    let n = decode_name(leaf).trim().to_ascii_lowercase();
    Some(match n.as_str() {
        "sent" | "sent items" | "sent messages" | "sent mail" | "sent-mail" | "sent_items" | "outbox-sent" => "sentitems",
        "drafts" | "draft" => "drafts",
        "trash" | "deleted" | "deleted items" | "deleted messages" | "bin" | "recycle bin" | "deleted_items" => "deleteditems",
        "junk" | "junk e-mail" | "junk email" | "junk mail" | "spam" | "bulk mail" | "bulk" => "junkemail",
        "archive" | "archives" | "all mail" => "archive",
        _ => return None,
    })
}

async fn list_boxes(c: &mut Conn) -> Result<(Vec<Mailbox>, String)> {
    let s = &mut c.s;
    let names = tm(async move {
        let stream = s.list(Some(""), Some("*")).await?;
        stream.collect::<Vec<_>>().await.into_iter().collect::<async_imap::error::Result<Vec<_>>>()
    })
    .await?;
    c.drain();
    let gmail = c.gmail;
    let mut delim = "/".to_string();
    let mut found: Vec<(String, Option<&'static str>)> = vec![];
    for n in &names {
        if let Some(d) = n.delimiter() {
            delim = d.to_string();
        }
        let unusable = n.attributes().iter().any(|a| matches!(a, NameAttribute::NoSelect) || matches!(a, NameAttribute::Extension(x) if x.eq_ignore_ascii_case("\\NonExistent")));
        if !unusable {
            let mut wk = special_use(n.name(), n.attributes());
            if wk.is_none() && !gmail {
                let leaf = n.delimiter().and_then(|d| n.name().rsplit(d).next()).unwrap_or(n.name());
                wk = role_by_name(leaf);
            }
            found.push((n.name().to_string(), wk));
        }
    }
    let usable: HashSet<String> = found.iter().map(|(n, _)| n.clone()).collect();
    let boxes = found
        .into_iter()
        .map(|(name, wk)| {
            let (parent, leaf) = match name.rsplit_once(delim.as_str()) {
                Some((p, l)) => (Some(p.to_string()), l.to_string()),
                None => (None, name.clone()),
            };
            let display = if wk == Some("inbox") { "Inbox".to_string() } else { decode_name(&leaf) };
            // "[Gmail]" itself can't hold mail, so the folders inside it show at the top level.
            Mailbox { parent: parent.filter(|p| usable.contains(p)), display, well_known: wk, name }
        })
        .collect();
    Ok((boxes, delim))
}

/// What the app keeps per folder.
struct Folder {
    id: String,
    remote: String,
    well_known: Option<String>,
    mark: Option<Mark>,
}

/// A folder's sync bookmark.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Mark {
    validity: u32,
    next: u32,
    total: u32,
    modseq: Option<u64>,
}

impl Mark {
    fn parse(s: &str) -> Option<Mark> {
        let p: Vec<&str> = s.split(':').collect();
        Some(Mark {
            validity: p.first()?.parse().ok()?,
            next: p.get(1)?.parse().ok()?,
            total: p.get(2)?.parse().ok()?,
            modseq: p.get(3).and_then(|m| m.parse().ok()),
        })
    }
    fn text(&self) -> String {
        format!("{}:{}:{}:{}", self.validity, self.next, self.total, self.modseq.map(|m| m.to_string()).unwrap_or_default())
    }
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

fn state_get(account_id: &str, key: &str) -> Option<String> {
    storage::with_db(|db| Ok(db.query_row("SELECT value FROM account_state WHERE account_id=?1 AND key=?2", params![account_id, key], |r| r.get(0)).optional()?))
        .ok()
        .flatten()
}

/// Saves the folder list and drops folders that are gone from the server (their messages are tidied afterwards).
fn store_folders(account_id: &str, boxes: &[Mailbox]) -> Result<Vec<Folder>> {
    storage::with_db(|db| {
        let tx = db.unchecked_transaction()?;
        for b in boxes {
            tx.execute(
                "INSERT INTO folders(id,account_id,remote_id,parent_remote_id,display_name,well_known_name) VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(account_id,remote_id) DO UPDATE SET parent_remote_id=excluded.parent_remote_id, display_name=excluded.display_name,
                   well_known_name=excluded.well_known_name",
                params![uuid::Uuid::new_v4().to_string(), account_id, b.name, b.parent, b.display, b.well_known],
            )?;
        }
        let keep: HashSet<&str> = boxes.iter().map(|b| b.name.as_str()).collect();
        let all: Vec<(String, String)> = {
            let mut st = tx.prepare("SELECT id, remote_id FROM folders WHERE account_id=?1")?;
            let rows = st.query_map([account_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.filter_map(|r| r.ok()).collect()
        };
        for (id, rid) in &all {
            if !keep.contains(rid.as_str()) {
                // Local mail still filed here moves to the account's Inbox until the next tidy-up sorts it.
                tx.execute(
                    "UPDATE messages SET folder_id=COALESCE((SELECT id FROM folders WHERE account_id=?1 AND well_known_name='inbox'), folder_id) WHERE folder_id=?2",
                    params![account_id, id],
                )?;
                tx.execute("DELETE FROM folders WHERE id=?1", [id])?;
            }
        }
        tx.commit()?;
        let mut st = db.prepare("SELECT id, remote_id, well_known_name, delta_link FROM folders WHERE account_id=?1")?;
        let rows = st.query_map([account_id], |r| {
            Ok(Folder { id: r.get(0)?, remote: r.get(1)?, well_known: r.get(2)?, mark: r.get::<_, Option<String>>(3)?.as_deref().and_then(Mark::parse) })
        })?;
        let mut list: Vec<Folder> = rows.filter_map(|r| r.ok()).collect();
        // Inbox first (new mail shows soonest), then real folders, then the views that repeat them (All Mail, Starred).
        let rank = |f: &Folder| match f.well_known.as_deref() {
            Some("inbox") => 0,
            Some("archive") => 3,
            Some("flagged" | "important") => 4,
            None => 2,
            _ => 1,
        };
        list.sort_by_key(rank);
        Ok(list)
    })
}

// ---------------------------------------------------------------- sync

/// UIDs as an IMAP set ("1:4,7,9:10").
fn uid_set(uids: impl IntoIterator<Item = u32>) -> String {
    let mut v: Vec<u32> = uids.into_iter().collect();
    v.sort_unstable();
    v.dedup();
    let mut out: Vec<String> = vec![];
    let mut i = 0;
    while i < v.len() {
        let start = v[i];
        while i + 1 < v.len() && v[i + 1] == v[i] + 1 {
            i += 1;
        }
        out.push(if v[i] == start { start.to_string() } else { format!("{start}:{}", v[i]) });
        i += 1;
    }
    out.join(",")
}

fn flags_of(f: &Fetch) -> (bool, bool, bool) {
    let (mut seen, mut flagged, mut draft) = (false, false, false);
    for fl in f.flags() {
        match fl {
            Flag::Seen => seen = true,
            Flag::Flagged => flagged = true,
            Flag::Draft => draft = true,
            _ => {}
        }
    }
    (seen, flagged, draft)
}

/// Updates read/flag state from the server, unless a change made here hasn't gone out yet. True when it changed.
fn set_flags(db: &rusqlite::Connection, message_id: &str, (seen, flagged, draft): (bool, bool, bool)) -> Result<bool> {
    Ok(db.execute(
        "UPDATE messages SET is_read=?2, is_flagged=?3, is_draft=?4 WHERE id=?1 AND sync_status='SYNCED' AND (is_read<>?2 OR is_flagged<>?3 OR is_draft<>?4)",
        params![message_id, seen, flagged, draft],
    )? > 0)
}

/// The local identity of a server message: Gmail's message id when there is one (so a message under several labels
/// is stored once), else its folder and UID.
fn key_of(account_id: &str, f: &Fetch, mailbox: &str, validity: u32, uid: u32) -> String {
    match f.gmail_msg_id() {
        Some(id) => format!("{account_id}:gm:{id}"),
        None => format!("{account_id}:{validity}:{uid}:{mailbox}"),
    }
}

fn link(db: &rusqlite::Connection, folder_id: &str, uid: u32, validity: u32, message_id: &str, size: Option<u32>) -> Result<()> {
    db.execute(
        "INSERT INTO imap_uids(folder_id,uid,validity,message_id,size) VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(folder_id,uid) DO UPDATE SET validity=excluded.validity, message_id=excluded.message_id, size=MAX(imap_uids.size, excluded.size)",
        params![folder_id, uid, validity, message_id, size.unwrap_or(0)],
    )?;
    Ok(())
}

/// Saves a new message from its headers; the body comes later. Returns its local id.
fn store_header(account_id: &str, folder_id: &str, key: &str, uid: u32, validity: u32, f: &Fetch) -> Result<String> {
    let header = f.header().unwrap_or_default();
    let parsed = MessageParser::default().parse_headers(header);
    let h = parsed.as_ref();
    let from = h.and_then(|m| m.from()).and_then(|a| a.first());
    // multipart/mixed almost always means attachments; the downloaded body settles it.
    let has_files = h.and_then(|m| m.content_type()).is_some_and(|c| c.ctype().eq_ignore_ascii_case("multipart") && c.subtype().is_some_and(|s| s.eq_ignore_ascii_case("mixed")));
    let received = f.internal_date().map(|d| d.timestamp()).or_else(|| h.and_then(|m| m.date()).map(|d| d.to_timestamp())).unwrap_or_else(|| chrono::Utc::now().timestamp());
    let (seen, flagged, draft) = flags_of(f);
    storage::with_db(|db| {
        let tx = db.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO messages(id,account_id,folder_id,remote_id,internet_message_id,subject,sender_name,sender_address,to_recipients_json,
                cc_recipients_json,snippet,received_at,is_read,is_flagged,is_draft,has_attachments,body_cached,inline_done)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'',?11,?12,?13,?14,?15,0,1)
             ON CONFLICT(remote_id) DO NOTHING",
            params![
                uuid::Uuid::new_v4().to_string(), account_id, folder_id, key, h.and_then(|m| m.message_id()), h.and_then(|m| m.subject()),
                from.and_then(|a| a.name()).filter(|n| !n.is_empty()), from.and_then(|a| a.address()),
                google::people(h.and_then(|m| m.to())), google::people(h.and_then(|m| m.cc())), received, seen, flagged, draft, has_files
            ],
        )?;
        let id: String = tx.query_row("SELECT id FROM messages WHERE remote_id=?1", [key], |r| r.get(0))?;
        set_flags(&tx, &id, (seen, flagged, draft))?;
        link(&tx, folder_id, uid, validity, &id, f.size)?;
        tx.commit()?;
        Ok(id)
    })
}

/// Downloads the headers of mail new to this folder. Mail already known from another label is only linked.
async fn add_new(c: &mut Conn, account_id: &str, f: &Folder, validity: u32, uids: Vec<u32>, fresh: &mut Vec<String>) -> Result<usize> {
    let mut n = 0;
    let gm = if c.gmail { " X-GM-MSGID" } else { "" };
    for chunk in uids.chunks(HEADER_CHUNK) {
        let light = c.uid_fetch(&uid_set(chunk.iter().copied()), &format!("(UID FLAGS RFC822.SIZE{gm})")).await?;
        let mut need = vec![];
        storage::with_db(|db| {
            for it in &light {
                let Some(uid) = it.uid else { continue };
                let key = key_of(account_id, it, &f.remote, validity, uid);
                match db.query_row("SELECT id FROM messages WHERE remote_id=?1", [&key], |r| r.get::<_, String>(0)).optional()? {
                    Some(id) => {
                        link(db, &f.id, uid, validity, &id, it.size)?;
                        set_flags(db, &id, flags_of(it))?;
                    }
                    None => need.push(uid),
                }
            }
            Ok(())
        })?;
        if need.is_empty() {
            continue;
        }
        let full = c.uid_fetch(&uid_set(need), &format!("(UID FLAGS INTERNALDATE RFC822.SIZE{gm} BODY.PEEK[HEADER])")).await?;
        for it in &full {
            let Some(uid) = it.uid else { continue };
            let id = store_header(account_id, &f.id, &key_of(account_id, it, &f.remote, validity, uid), uid, validity, it)?;
            if f.well_known.as_deref() == Some("inbox") {
                fresh.push(id);
            }
            n += 1;
        }
    }
    Ok(n)
}

/// Brings one folder up to date. Returns how many messages changed.
async fn sync_box(c: &mut Conn, account_id: &str, f: &Folder, now: &Mark, fresh: &mut Vec<String>) -> Result<usize> {
    let validity = c.open_box(&f.remote, false).await?;
    let reset = f.mark.map(|m| m.validity) != Some(validity);
    if reset {
        // First visit, or the server renumbered the folder (UIDVALIDITY changed): its old UIDs mean nothing now.
        storage::with_db(|db| Ok(db.execute("DELETE FROM imap_uids WHERE folder_id=?1", [&f.id]).map(|_| ())?))?;
    }
    let known: HashMap<u32, String> = storage::with_db(|db| {
        let mut st = db.prepare("SELECT uid, message_id FROM imap_uids WHERE folder_id=?1")?;
        let rows = st.query_map([&f.id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })?;
    let mut n = 0;

    // 1. Mail we already have here: flag changes, and what has gone.
    if !known.is_empty() {
        let mut present = HashSet::new();
        let mut items = vec![];
        let uids: Vec<u32> = known.keys().copied().collect();
        for chunk in uids.chunks(1000) {
            items.extend(c.uid_fetch(&uid_set(chunk.iter().copied()), "(UID FLAGS)").await?);
        }
        storage::with_db(|db| {
            let tx = db.unchecked_transaction()?;
            for it in &items {
                let Some(uid) = it.uid else { continue };
                present.insert(uid);
                if let Some(id) = known.get(&uid) {
                    if set_flags(&tx, id, flags_of(it))? {
                        n += 1;
                    }
                }
            }
            for uid in known.keys().filter(|u| !present.contains(u)) {
                tx.execute("DELETE FROM imap_uids WHERE folder_id=?1 AND uid=?2", params![f.id, uid])?;
                n += 1;
            }
            tx.commit()?;
            Ok(())
        })?;
    }

    // 2. New mail.
    let mut new_uids: Vec<u32> = match f.mark.filter(|_| !reset) {
        None => {
            let since = (chrono::Utc::now() - chrono::Duration::days(INITIAL_DAYS)).format("%d-%b-%Y");
            let mut u = c.uid_search(&format!("SINCE {since}")).await?;
            u.sort_unstable_by(|a, b| b.cmp(a));
            u.truncate(INITIAL_MAX);
            u
        }
        Some(old) if now.next > old.next => {
            let items = c.uid_fetch(&format!("{}:*", old.next), "(UID)").await?;
            items.iter().filter_map(|i| i.uid).filter(|u| *u >= old.next).collect()
        }
        Some(_) => vec![],
    };
    new_uids.retain(|u| !known.contains_key(u));
    n += add_new(c, account_id, f, validity, new_uids, fresh).await?;
    Ok(n)
}

/// After the folders are synced: each message's primary folder and labels follow where the server keeps it, and
/// messages the server no longer has anywhere are removed (unless a change made here is still waiting to go out).
fn tidy(account_id: &str) -> Result<()> {
    storage::with_db(|db| {
        let tx = db.unchecked_transaction()?;
        let orphans = "SELECT id FROM messages m WHERE account_id=?1 AND sync_status='SYNCED' AND NOT EXISTS (SELECT 1 FROM imap_uids u WHERE u.message_id=m.id)";
        tx.execute(&format!("DELETE FROM messages_fts WHERE message_id IN ({orphans})"), [account_id])?;
        tx.execute(&format!("DELETE FROM messages WHERE id IN ({orphans})"), [account_id])?;
        tx.execute(
            "UPDATE messages SET folder_id=(
                SELECT u.folder_id FROM imap_uids u JOIN folders f ON f.id=u.folder_id WHERE u.message_id=messages.id
                ORDER BY CASE f.well_known_name WHEN 'deleteditems' THEN 0 WHEN 'junkemail' THEN 1 WHEN 'drafts' THEN 2 WHEN 'inbox' THEN 3
                  WHEN 'sentitems' THEN 4 WHEN 'archive' THEN 6 WHEN 'flagged' THEN 7 WHEN 'important' THEN 7 ELSE 5 END, f.display_name LIMIT 1)
             WHERE account_id=?1 AND sync_status='SYNCED' AND EXISTS (SELECT 1 FROM imap_uids u WHERE u.message_id=messages.id)",
            [account_id],
        )?;
        tx.execute("DELETE FROM message_labels WHERE message_id IN (SELECT id FROM messages WHERE account_id=?1 AND sync_status='SYNCED')", [account_id])?;
        tx.execute(
            "INSERT OR IGNORE INTO message_labels(message_id,folder_id)
             SELECT u.message_id, u.folder_id FROM imap_uids u JOIN messages m ON m.id=u.message_id WHERE m.account_id=?1 AND m.sync_status='SYNCED'",
            [account_id],
        )?;
        tx.commit()?;
        Ok(())
    })
}

fn account_email(account_id: &str) -> Result<String> {
    storage::with_db(|db| Ok(db.query_row("SELECT email FROM accounts WHERE id=?1", [account_id], |r| r.get(0))?))
}

/// One full pass for an IMAP account (runs inside auth::in_account for that account).
pub async fn sync_account(account_id: &str) -> Result<sync::SyncReport> {
    let email = account_email(account_id)?;
    let mut c = take(&email, Use::Background).await?;
    let r = sync_with(&mut c, account_id).await;
    done(&email, Use::Background, c, &r);
    r
}

async fn sync_with(c: &mut Conn, account_id: &str) -> Result<sync::SyncReport> {
    let mut rep = sync::SyncReport::default();
    let first_time = state_get(account_id, "imap_synced").is_none();
    let (boxes, delim) = list_boxes(c).await?;
    state_set(account_id, "imap_delimiter", &delim)?;
    let folders = store_folders(account_id, &boxes)?;
    rep.folders = folders.len();
    let items = if c.condstore { "(MESSAGES UNSEEN UIDNEXT UIDVALIDITY HIGHESTMODSEQ)" } else { "(MESSAGES UNSEEN UIDNEXT UIDVALIDITY)" };
    let mut fresh = vec![];
    for f in &folders {
        let st = match tm(c.s.status(&f.remote, items)).await {
            Ok(st) => st,
            // Deleted on the server since the folder list was read: the next pass drops it.
            Err(e) if matches!(e.downcast_ref::<async_imap::error::Error>(), Some(async_imap::error::Error::No(_))) => continue,
            Err(e) => return Err(e),
        };
        c.drain();
        storage::with_db(|db| {
            Ok(db.execute("UPDATE folders SET total_count=?1, unread_count=?2 WHERE id=?3", params![st.exists, st.unseen.unwrap_or(0), f.id]).map(|_| ())?)
        })?;
        let now = Mark { validity: st.uid_validity.unwrap_or(0), next: st.uid_next.unwrap_or(0), total: st.exists, modseq: st.highest_modseq };
        // Nothing new, nothing gone, no flag changed: skip the folder. (Without CONDSTORE flag changes can't be seen
        // from here, so such servers always get the cheap flags check.)
        if f.mark == Some(now) && now.modseq.is_some() {
            continue;
        }
        rep.messages_changed += sync_box(c, account_id, f, &now, &mut fresh).await?;
        storage::with_db(|db| Ok(db.execute("UPDATE folders SET delta_link=?1 WHERE id=?2", params![now.text(), f.id]).map(|_| ())?))?;
    }
    tidy(account_id)?;
    rep.bodies_downloaded = hydrate(c, account_id, BODIES_PER_PASS).await?;
    // Rules for this account live on this computer: they run on new Inbox mail as it arrives.
    if !first_time && !fresh.is_empty() {
        if let Err(e) = crate::rules::apply_local(account_id, &fresh) {
            eprintln!("[graphite] rules for new mail failed: {e}");
        }
    }
    if first_time {
        state_set(account_id, "imap_synced", "1")?;
        let _ = crate::notify::collect_new(); // the first download must not flood the desktop with notifications
    }
    Ok(rep)
}

// ---------------------------------------------------------------- bodies

/// Saves a downloaded message: its text for reading and search, inline pictures, and the list of attachments.
fn store_body(local_id: &str, raw: &[u8]) -> Result<()> {
    let p = google::parse_raw(raw)?;
    let hash = sync::write_blob(p.html.as_bytes(), "html")?;
    let inline_hashes: Vec<String> = p.inline.iter().map(|i| sync::write_blob(&i.bytes, "bin")).collect::<Result<_>>()?;
    let text = sync::strip_html(&p.html);
    let snippet: String = text.chars().take(200).collect();
    storage::with_db(|db| {
        let tx = db.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE messages SET body_blob_hash=?1, body_cached=1, inline_done=1, snippet=?2, has_attachments=?3 WHERE id=?4",
            params![hash, snippet, !p.files.is_empty(), local_id],
        )?;
        if changed == 0 {
            return Ok(()); // deleted meanwhile
        }
        tx.execute("DELETE FROM attachments WHERE message_id=?1", [local_id])?;
        for (i, h) in p.inline.iter().zip(&inline_hashes) {
            tx.execute(
                "INSERT INTO attachments(id,message_id,filename,mime_type,size_bytes,content_id,is_inline,blob_hash,is_downloaded) VALUES(?1,?2,?3,?4,?5,?6,1,?7,1)",
                params![uuid::Uuid::new_v4().to_string(), local_id, i.name, i.mime, i.bytes.len() as i64, i.cid, h],
            )?;
        }
        for (name, mime, size) in &p.files {
            tx.execute(
                "INSERT INTO attachments(id,message_id,filename,mime_type,size_bytes,is_inline,is_downloaded) VALUES(?1,?2,?3,?4,?5,0,0)",
                params![uuid::Uuid::new_v4().to_string(), local_id, name, mime, *size as i64],
            )?;
        }
        let (subject, name, addr): (String, String, String) = tx.query_row(
            "SELECT COALESCE(subject,''), COALESCE(sender_name,''), COALESCE(sender_address,'') FROM messages WHERE id=?1",
            [local_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        tx.execute("DELETE FROM messages_fts WHERE message_id=?1", [local_id])?;
        tx.execute(
            "INSERT INTO messages_fts(message_id,subject,sender_name,sender_address,body_plaintext) VALUES(?1,?2,?3,?4,?5)",
            params![local_id, subject, name, addr, text],
        )?;
        tx.commit()?;
        Ok(())
    })
}

/// Downloads bodies newest first, a few per command, so recent mail can be read offline.
async fn hydrate(c: &mut Conn, account_id: &str, limit: usize) -> Result<usize> {
    let todo: Vec<(String, String, u32, u32, i64)> = storage::with_db(|db| {
        let mut st = db.prepare(
            "SELECT m.id, f.remote_id, u.uid, u.validity, u.size FROM messages m
             JOIN imap_uids u ON u.rowid=(SELECT u2.rowid FROM imap_uids u2 WHERE u2.message_id=m.id ORDER BY u2.folder_id<>m.folder_id LIMIT 1)
             JOIN folders f ON f.id=u.folder_id
             WHERE m.account_id=?1 AND m.body_cached=0 AND u.size<=?2 ORDER BY m.received_at DESC LIMIT ?3",
        )?;
        let rows = st.query_map(params![account_id, BACKGROUND_MAX_BYTES, limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })?;
    let mut by_box: HashMap<(String, u32), Vec<(u32, String, i64)>> = HashMap::new();
    for (id, mailbox, uid, validity, size) in todo {
        by_box.entry((mailbox, validity)).or_default().push((uid, id, size));
    }
    let mut n = 0;
    for ((mailbox, validity), list) in by_box {
        if c.open_box(&mailbox, false).await? != validity {
            continue; // renumbered since; the next sync links these again
        }
        for chunk in body_batches(&list) {
            let ids: HashMap<u32, &String> = chunk.iter().map(|(u, id, _)| (*u, id)).collect();
            for it in c.uid_fetch(&uid_set(ids.keys().copied()), "(UID BODY.PEEK[])").await? {
                if let (Some(uid), Some(raw)) = (it.uid, it.body()) {
                    if let Some(id) = ids.get(&uid) {
                        match store_body(id, raw) {
                            Ok(()) => n += 1,
                            Err(e) => eprintln!("[graphite] imap message skipped: {e}"),
                        }
                    }
                }
            }
        }
    }
    Ok(n)
}

/// Splits a list of (uid, id, size) into downloads of at most BODY_CHUNK messages and about BODY_CHUNK_BYTES.
fn body_batches<T>(list: &[(u32, T, i64)]) -> Vec<&[(u32, T, i64)]> {
    let mut out = vec![];
    let (mut start, mut bytes) = (0, 0);
    for (i, item) in list.iter().enumerate() {
        if i > start && (i - start >= BODY_CHUNK || bytes + item.2 > BODY_CHUNK_BYTES) {
            out.push(&list[start..i]);
            (start, bytes) = (i, 0);
        }
        bytes += item.2;
    }
    if start < list.len() {
        out.push(&list[start..]);
    }
    out
}

/// Where a message is on the server.
struct Location {
    mailbox: String,
    uid: u32,
    validity: u32,
    kind: Option<String>,
    folder_id: String,
}

/// A location of a message: in `prefer` (a mailbox name) when it is there, else in its own folder, else anywhere.
fn location(message_id: &str, prefer: Option<&str>) -> Result<Option<Location>> {
    storage::with_db(|db| {
        Ok(db
            .query_row(
                "SELECT f.remote_id, u.uid, u.validity, f.well_known_name, f.id FROM imap_uids u JOIN folders f ON f.id=u.folder_id WHERE u.message_id=?1
                 ORDER BY f.remote_id=COALESCE(?2,'') DESC, u.folder_id=(SELECT folder_id FROM messages WHERE id=?1) DESC,
                   COALESCE(f.well_known_name,'') IN ('archive','flagged','important') ASC LIMIT 1",
                params![message_id, prefer],
                |r| Ok(Location { mailbox: r.get(0)?, uid: r.get(1)?, validity: r.get(2)?, kind: r.get(3)?, folder_id: r.get(4)? }),
            )
            .optional()?)
    })
}

/// Opens the mailbox of `loc`, making sure its UIDs still mean what they meant at the last sync.
async fn open_checked(c: &mut Conn, loc: &Location, write: bool) -> Result<()> {
    if c.open_box(&loc.mailbox, write).await? != loc.validity {
        return Err(refused(503, "The folder was renumbered on the server; this change is tried again after the next sync."));
    }
    Ok(())
}

async fn fetch_raw(c: &mut Conn, loc: &Location) -> Result<Vec<u8>> {
    open_checked(c, loc, false).await?;
    let items = c.uid_fetch(&loc.uid.to_string(), "(UID BODY.PEEK[])").await?;
    items.iter().find(|i| i.uid == Some(loc.uid)).and_then(|i| i.body()).map(|b| b.to_vec()).ok_or_else(|| refused(404, "That message is no longer on the server."))
}

/// (account email, body downloaded, account is IMAP) of a message.
fn owner(message_id: &str) -> Result<(String, bool, bool)> {
    storage::with_db(|db| {
        db.query_row(
            "SELECT a.email, m.body_cached, COALESCE(a.provider,'')=?2 FROM messages m JOIN accounts a ON a.id=m.account_id WHERE m.id=?1",
            params![message_id, PROVIDER],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or(anyhow!("That message no longer exists."))
    })
}

/// Opening a message whose body isn't here yet: download it now. Messages of other account types are left alone.
pub async fn ensure_body(message_id: &str) -> Result<()> {
    let (email, cached, ours) = owner(message_id)?;
    if cached || !ours {
        return Ok(());
    }
    let loc = location(message_id, None)?.ok_or(anyhow!("That message is no longer on the server."))?;
    let mut c = take(&email, Use::Reading).await?;
    let r = fetch_raw(&mut c, &loc).await;
    done(&email, Use::Reading, c, &r);
    store_body(message_id, &r?)
}

/// One attachment of a message (found by name and size in the whole message).
pub async fn download_attachment(message_id: &str, name: &str, size: i64) -> Result<Vec<u8>> {
    let (email, _, _) = owner(message_id)?;
    let loc = location(message_id, None)?.ok_or(anyhow!("That message is no longer on the server."))?;
    let mut c = take(&email, Use::Reading).await?;
    let r = fetch_raw(&mut c, &loc).await;
    done(&email, Use::Reading, c, &r);
    let p = google::parse_raw_full(&r?, true)?;
    let idx = p.files.iter().position(|(n, _, s)| n == name && *s as i64 == size).or_else(|| p.files.iter().position(|(n, _, _)| n == name));
    idx.and_then(|i| p.file_bytes.get(i).cloned()).ok_or_else(|| anyhow!("That attachment is no longer in the message."))
}

// ---------------------------------------------------------------- offline queue operations

/// Runs one queued change against the IMAP/SMTP servers. Same contract as the other providers: an HTTP-like
/// status (and a body the queue only reads for error messages); Err means out of reach, try again later.
pub async fn apply(op: &str, entity: &str, p: &Value) -> Result<(u16, Value)> {
    let email = auth::active_email().ok_or(anyhow!("Not signed in"))?;
    let r = if op == "SEND" {
        send(&email, entity, p).await
    } else {
        match take(&email, Use::Background).await {
            Ok(mut c) => {
                let r = run(&mut c, op, entity, p).await;
                done(&email, Use::Background, c, &r);
                r
            }
            Err(e) => Err(e),
        }
    };
    match r {
        Ok(()) => Ok((200, Value::Null)),
        Err(e) => {
            if let Some(x) = e.downcast_ref::<Refused>() {
                return Ok((x.status, json!({ "error": { "message": x.message } })));
            }
            if let Some(async_imap::error::Error::No(m) | async_imap::error::Error::Bad(m)) = e.downcast_ref::<async_imap::error::Error>() {
                return Ok((400, json!({ "error": { "message": format!("The mail server refused: {m}") } })));
            }
            Err(e)
        }
    }
}

fn gone() -> anyhow::Error {
    refused(404, "That message is no longer on the server.")
}

fn folder_kind(account_of_message: &str, mailbox: &str) -> Result<Option<(String, Option<String>)>> {
    storage::with_db(|db| {
        Ok(db
            .query_row(
                "SELECT f.id, f.well_known_name FROM folders f WHERE f.remote_id=?2 AND f.account_id=(SELECT a.id FROM accounts a WHERE lower(a.email)=lower(?1))",
                params![account_of_message, mailbox],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    })
}

/// Another queued change for this message is waiting behind the current one.
fn more_queued(entity: &str) -> bool {
    storage::with_db(|db| Ok(db.query_row("SELECT COUNT(*) FROM offline_mutation_queue WHERE local_entity_id=?1", [entity], |r| r.get::<_, i64>(0))? > 1)).unwrap_or(false)
}

/// After a message was moved or copied: find its new UID (by Message-ID) so further queued changes reach it.
async fn relink(c: &mut Conn, entity: &str, dest: &str, dest_folder: &str) -> Result<()> {
    let mid: Option<String> = storage::with_db(|db| Ok(db.query_row("SELECT internet_message_id FROM messages WHERE id=?1", [entity], |r| r.get(0)).optional()?.flatten()))?;
    let Some(mid) = mid.map(|m| m.replace(['"', '\\', '\r', '\n'], "")).filter(|m| !m.is_empty()) else { return Ok(()) };
    let mid = if mid.starts_with('<') { mid } else { format!("<{mid}>") };
    let validity = c.open_box(dest, false).await?;
    if let Some(uid) = c.uid_search(&format!("HEADER Message-ID \"{mid}\"")).await?.into_iter().max() {
        storage::with_db(|db| link(db, dest_folder, uid, validity, entity, None))?;
    }
    Ok(())
}

async fn run(c: &mut Conn, op: &str, entity: &str, p: &Value) -> Result<()> {
    let email = auth::active_email().unwrap_or_default();
    match op {
        "SET_READ" | "SET_FLAG" => {
            let (flag, on) = if op == "SET_READ" { ("\\Seen", p["read"].as_bool() == Some(true)) } else { ("\\Flagged", p["flagged"].as_bool() == Some(true)) };
            let loc = location(entity, None)?.ok_or_else(gone)?;
            open_checked(c, &loc, true).await?;
            c.uid_store(&loc.uid.to_string(), &format!("{}FLAGS.SILENT ({flag})", if on { "+" } else { "-" })).await
        }
        "MOVE" | "COPY" => {
            let dest = p["destination"].as_str().unwrap_or_default();
            let (dest_folder, dest_kind) = folder_kind(&email, dest)?.ok_or_else(|| refused(400, "That folder no longer exists."))?;
            let source = p["source"].as_str().filter(|s| !s.is_empty());
            let loc = location(entity, source)?.ok_or_else(gone)?;
            if loc.mailbox == dest {
                return Ok(());
            }
            let leaving = op == "MOVE";
            // Gmail: a message "in" All Mail / Starred / Important is only shown there. Taking it out of such a view would
            // delete or unstar it, so filing it elsewhere means adding the label (Trash and Spam still move it).
            let view = matches!(loc.kind.as_deref(), Some("archive" | "flagged" | "important")) || source.is_some_and(|s| s != loc.mailbox);
            let into_bin = matches!(dest_kind.as_deref(), Some("deleteditems" | "junkemail"));
            open_checked(c, &loc, leaving).await?;
            let uid = loc.uid.to_string();
            if !leaving || (c.gmail && view && !into_bin) {
                tm(c.s.uid_copy(&uid, dest)).await?;
            } else if c.can_move {
                tm(c.s.uid_mv(&uid, dest)).await?;
            } else {
                tm(c.s.uid_copy(&uid, dest)).await?;
                c.uid_store(&uid, "+FLAGS.SILENT (\\Deleted)").await?;
                c.expunge(loc.uid).await?;
            }
            c.drain();
            if leaving && !(c.gmail && view && !into_bin) {
                storage::with_db(|db| Ok(db.execute("DELETE FROM imap_uids WHERE folder_id=?1 AND uid=?2", params![loc.folder_id, loc.uid]).map(|_| ())?))?;
            }
            if more_queued(entity) {
                relink(c, entity, dest, &dest_folder).await?;
            }
            Ok(())
        }
        "DELETE_HARD" => {
            let trash: Option<String> = storage::with_db(|db| {
                Ok(db
                    .query_row(
                        "SELECT f.remote_id FROM folders f JOIN imap_uids u ON u.folder_id=f.id WHERE u.message_id=?1 AND f.well_known_name='deleteditems'",
                        [entity],
                        |r| r.get(0),
                    )
                    .optional()?)
            })?;
            let loc = location(entity, trash.as_deref())?.ok_or_else(gone)?;
            open_checked(c, &loc, true).await?;
            c.uid_store(&loc.uid.to_string(), "+FLAGS.SILENT (\\Deleted)").await?;
            c.expunge(loc.uid).await?;
            storage::with_db(|db| Ok(db.execute("DELETE FROM imap_uids WHERE message_id=?1", [entity]).map(|_| ())?))
        }
        _ => Err(refused(400, "This change isn't available for app-password accounts.")),
    }
}

// ---------------------------------------------------------------- sending

fn transport(email: &str, pw: &Secret) -> Result<lettre::AsyncSmtpTransport<lettre::Tokio1Executor>> {
    transport_to(email, pw, settings_of(email).as_ref())
}

fn transport_to(email: &str, pw: &Secret, settings: Option<&ImapSettings>) -> Result<lettre::AsyncSmtpTransport<lettre::Tokio1Executor>> {
    use lettre::transport::smtp::authentication::Credentials;
    let srv = smtp_server_with(email, settings);
    let builder = match srv.tls {
        Tls::Implicit => lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&srv.host)?.port(srv.port),
        Tls::StartTls => lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::starttls_relay(&srv.host)?.port(srv.port),
        Tls::Plain => lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::builder_dangerous(&srv.host).port(srv.port),
    };
    Ok(builder.credentials(Credentials::new(srv.user, pw.expose().to_string())).timeout(Some(NET_TIMEOUT)).build())
}

async fn send(email: &str, entity: &str, p: &Value) -> Result<()> {
    use lettre::AsyncTransport;
    let list = |k: &str| p[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default();
    let (to, cc, bcc) = (list("to"), list("cc"), list("bcc"));
    let name: String = storage::with_db(|db| {
        Ok(db.query_row("SELECT display_name FROM accounts WHERE lower(email)=lower(?1)", [email], |r| r.get::<_, Option<String>>(0)).optional()?.flatten())
    })?
    .filter(|n| !n.eq_ignore_ascii_case(email))
    .unwrap_or_default();
    let mode = p["mode"].as_str().unwrap_or("new");
    // Replies join the original conversation (In-Reply-To / References).
    let original: Option<String> = if matches!(mode, "reply" | "replyAll") {
        storage::with_db(|db| Ok(db.query_row("SELECT internet_message_id FROM messages WHERE id=?1", [entity], |r| r.get(0)).optional()?.flatten()))?
    } else {
        None
    };
    let mut files: Vec<google::OutFile> = vec![];
    for a in p["attachments"].as_array().cloned().unwrap_or_default() {
        let bytes = sync::read_blob(a["blob"].as_str().unwrap_or_default(), "bin")?;
        files.push((a["name"].as_str().unwrap_or("attachment").to_string(), a["mime"].as_str().unwrap_or("application/octet-stream").to_string(), bytes));
    }
    // A forward carries the original's attachments.
    if mode == "forward" {
        if let Some(loc) = location(entity, None)? {
            let mut c = take(email, Use::Reading).await?;
            let r = fetch_raw(&mut c, &loc).await;
            done(email, Use::Reading, c, &r);
            let orig = google::parse_raw_full(&r?, true)?;
            for ((n, m, _), b) in orig.files.into_iter().zip(orig.file_bytes) {
                files.push((n, m, b));
            }
        }
    }
    // Blind copies never go into the message: they are SMTP envelope recipients only (RCPT TO), so no header names them.
    let raw = google::build_raw((&name, email), &to, &cc, &[], p["subject"].as_str().unwrap_or_default(), p["html"].as_str().unwrap_or_default(), original.as_deref().map(|m| (m, "")), &files)?;
    let parse = |a: &str| a.trim().parse::<lettre::Address>().map_err(|_| refused(400, format!("“{a}” is not a valid email address.")));
    let rcpts = to.iter().chain(&cc).chain(&bcc).map(|a| parse(a)).collect::<Result<Vec<_>>>()?;
    if rcpts.is_empty() {
        return Err(refused(400, "The message has no recipients."));
    }
    let envelope = lettre::address::Envelope::new(Some(parse(email)?), rcpts).map_err(|e| refused(400, e.to_string()))?;
    let pw = password(email)?;
    // Gmail files what it sends in Sent Mail by itself; the next sync brings it in.
    transport(email, &pw)?.send_raw(&envelope, &raw).await.map_err(|e| smtp_problem(email, &pw, e))?;
    Ok(())
}

// ---------------------------------------------------------------- folders (create / rename / delete)

fn folder_row(local_id: &str) -> Result<(String, String, Option<String>, String)> {
    // (remote name, account id, well-known name, account email)
    storage::with_db(|db| {
        db.query_row(
            "SELECT f.remote_id, f.account_id, f.well_known_name, a.email FROM folders f JOIN accounts a ON a.id=f.account_id WHERE f.id=?1",
            [local_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or(anyhow!("That folder no longer exists."))
    })
}

fn clean_name(name: &str, delim: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        bail!("Enter a folder name.");
    }
    if n.contains(delim) {
        bail!("A folder name can't contain “{delim}”.");
    }
    if n.chars().count() > 200 {
        bail!("That name is too long.");
    }
    Ok(n.to_string())
}

/// Runs one folder change on the account's background connection.
async fn with_background<T>(email: &str, f: impl AsyncFnOnce(&mut Conn) -> Result<T>) -> Result<T> {
    let mut c = take(email, Use::Background).await?;
    let r = f(&mut c).await;
    done(email, Use::Background, c, &r);
    r.map_err(|e| match e.downcast_ref::<async_imap::error::Error>() {
        Some(async_imap::error::Error::No(m) | async_imap::error::Error::Bad(m)) => anyhow!("Gmail refused: {m}"),
        _ => e,
    })
}

pub async fn create_folder(name: &str, parent: Option<&str>, account_id: &str) -> Result<String> {
    let email = account_email(account_id)?;
    let delim = state_get(account_id, "imap_delimiter").unwrap_or_else(|| "/".into());
    let name = clean_name(name, &delim)?;
    let parent_remote = match parent {
        Some(p) => Some(folder_row(p)?.0),
        None => None,
    };
    let full = match &parent_remote {
        Some(p) => format!("{p}{delim}{}", encode_name(&name)),
        None => encode_name(&name),
    };
    with_background(&email, async |c| tm(c.s.create(&full)).await).await?;
    storage::with_db(|db| {
        db.execute(
            "INSERT INTO folders(id,account_id,remote_id,parent_remote_id,display_name) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account_id,remote_id) DO NOTHING",
            params![uuid::Uuid::new_v4().to_string(), account_id, full, parent_remote, name],
        )?;
        Ok(db.query_row("SELECT id FROM folders WHERE account_id=?1 AND remote_id=?2", params![account_id, full], |r| r.get(0))?)
    })
}

pub async fn rename_folder(local_id: &str, name: &str) -> Result<()> {
    let (old, account_id, wk, email) = folder_row(local_id)?;
    if wk.is_some() {
        bail!("Built-in folders like Inbox can't be renamed.");
    }
    let delim = state_get(&account_id, "imap_delimiter").unwrap_or_else(|| "/".into());
    let name = clean_name(name, &delim)?;
    let new = match old.rsplit_once(delim.as_str()) {
        Some((parent, _)) => format!("{parent}{delim}{}", encode_name(&name)),
        None => encode_name(&name),
    };
    with_background(&email, async |c| {
        tm(c.s.rename(&old, &new)).await
    })
    .await?;
    // The folders inside it are renamed along with it on the server; follow here.
    storage::with_db(|db| {
        let tx = db.unchecked_transaction()?;
        let len = old.len() as i64;
        tx.execute(
            "UPDATE folders SET remote_id=?3 || substr(remote_id, ?4+1) WHERE account_id=?1 AND (remote_id=?2 OR substr(remote_id,1,?4+?6)=?2 || ?5)",
            params![account_id, old, new, len, delim, delim.len() as i64],
        )?;
        tx.execute(
            "UPDATE folders SET parent_remote_id=?3 || substr(parent_remote_id, ?4+1) WHERE account_id=?1 AND (parent_remote_id=?2 OR substr(parent_remote_id,1,?4+?6)=?2 || ?5)",
            params![account_id, old, new, len, delim, delim.len() as i64],
        )?;
        tx.execute("UPDATE folders SET display_name=?1 WHERE id=?2", params![name, local_id])?;
        tx.commit()?;
        Ok(())
    })
}

pub async fn delete_folder(local_id: &str) -> Result<()> {
    let (remote, account_id, wk, email) = folder_row(local_id)?;
    if wk.is_some() {
        bail!("Built-in folders like Inbox can't be deleted.");
    }
    let delim = state_get(&account_id, "imap_delimiter").unwrap_or_else(|| "/".into());
    // Deepest first, so no folder is deleted while it still has folders inside.
    let mut doomed: Vec<(String, String)> = storage::with_db(|db| {
        let mut st = db.prepare("SELECT id, remote_id FROM folders WHERE account_id=?1 AND (remote_id=?2 OR substr(remote_id,1,length(?2)+length(?3))=?2 || ?3)")?;
        let rows = st.query_map(params![account_id, remote, delim], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })?;
    doomed.sort_by_key(|(_, r)| std::cmp::Reverse(r.len()));
    let names: Vec<String> = doomed.iter().map(|(_, r)| r.clone()).collect();
    with_background(&email, async |c| {
        if c.selected.take().is_some() {
            tm(c.s.close()).await.ok(); // leave the open mailbox before deleting it
        }
        for n in &names {
            tm(c.s.delete(n)).await?;
        }
        Ok(())
    })
    .await?;
    storage::with_db(|db| {
        for (id, _) in &doomed {
            db.execute("DELETE FROM folders WHERE id=?1", [id])?;
        }
        Ok(())
    })?;
    tidy(&account_id)
}

#[cfg(test)]
#[path = "imap_tests.rs"]
mod tests;
