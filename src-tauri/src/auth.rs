//! Microsoft identity: OAuth2 Authorization Code + PKCE via loopback redirect.
//! Refresh tokens live only in the OS keyring: GNOME Keyring / KWallet via Secret Service on Linux, the Windows
//! Credential Manager on Windows (the `keyring` crate picks the backend for the platform it is built on).

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const CLIENT_ID: &str = "4fd86bd0-7f26-4941-87a6-e69aa7e982d0";
const AUTHORITY: &str = "https://login.microsoftonline.com/common/oauth2/v2.0";

/// The identity endpoints ("common": work, school and personal accounts alike); tests point it at a pretend server.
fn authority() -> String {
    #[cfg(test)]
    if let Ok(a) = std::env::var("GM_TEST_AUTHORITY") {
        return a;
    }
    AUTHORITY.to_string()
}

/// Microsoft Graph's base URL; tests point it at a pretend server.
fn graph() -> String {
    #[cfg(test)]
    if let Ok(g) = std::env::var("GM_TEST_GRAPH") {
        return g;
    }
    "https://graph.microsoft.com/v1.0".to_string()
}
const SCOPES: &str =
    "openid profile offline_access User.Read Mail.ReadWrite Mail.Send Calendars.ReadWrite";
/// Extra permission needed to create Outlook mail rules; asked for only when the user turns rules on.
const RULES_SCOPE: &str = "MailboxSettings.ReadWrite";

/// Extra permissions for the Contacts view, asked for only when the user grants them (Contacts → Grant access):
/// personal contacts, the people they work with, and the organisation's directory (basic profiles).
pub const CONTACTS_SCOPES: &str = "Contacts.Read People.Read User.ReadBasic.All";
/// The same for personal Microsoft accounts (outlook.com, hotmail.com, live.com): no directory.
pub const CONTACTS_SCOPES_PERSONAL: &str = "Contacts.Read People.Read";

/// Error codes Microsoft returns when an organisation's administrator must approve the app before anyone in it can
/// sign in (AADSTS90094 / AADSTS90095 "admin consent required", AADSTS65001 "has not consented", or the authorize
/// endpoint's `admin_consent_required` error). Personal accounts never produce these.
pub fn admin_approval_needed(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("aadsts90094") || t.contains("aadsts90095") || t.contains("aadsts65001") || t.contains("admin_consent_required")
        || t.contains("need admin approval") || t.contains("needs admin approval")
}

/// Prefix of the error text the UI looks for to show the admin-approval explanation instead of a plain error.
pub const ADMIN_APPROVAL_MARK: &str = "admin-approval:";

pub const ADMIN_APPROVAL_MESSAGE: &str =
    "Your organisation requires an administrator to approve Graphite Mail before you can sign in. \
     This is done once for everyone: send the link to your IT admin, and sign in again after they have approved it.";

/// Turns a sign-in failure caused by missing admin consent into a marked error the screens recognise; every other
/// error is passed through unchanged. Never includes tokens or codes.
fn mark_admin_approval(e: anyhow::Error) -> anyhow::Error {
    if admin_approval_needed(&e.to_string()) { anyhow!("{ADMIN_APPROVAL_MARK}{ADMIN_APPROVAL_MESSAGE}") } else { e }
}

/// The link an administrator opens to approve Graphite Mail for their organisation (Microsoft's admin consent page
/// for this app's client id). With the user's address the tenant is their domain, so the admin lands in the right
/// directory; otherwise "organizations" lets them pick. It asks for the same permissions users would be asked for.
pub fn admin_consent_url(email: Option<&str>) -> String {
    let tenant = email
        .and_then(|e| e.rsplit_once('@').map(|(_, d)| d.trim().to_lowercase()))
        .filter(|d| !d.is_empty() && !["outlook.com", "hotmail.com", "live.com", "msn.com", "gmail.com"].contains(&d.as_str()))
        .unwrap_or_else(|| "organizations".into());
    let mut u = url::Url::parse(&format!("https://login.microsoftonline.com/{tenant}/v2.0/adminconsent")).expect("fixed URL");
    u.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("scope", &scopes(true, true, false).split_whitespace().filter(|s| !matches!(*s, "openid" | "profile" | "offline_access")).map(|s| format!("https://graph.microsoft.com/{s}")).collect::<Vec<_>>().join(" "))
        .append_pair("redirect_uri", "http://localhost")
        .append_pair("state", "graphite-mail-admin-consent");
    u.to_string()
}

fn scopes(rules: bool, contacts: bool, personal: bool) -> String {
    let mut s = SCOPES.to_string();
    if rules {
        s = format!("{s} {RULES_SCOPE}");
    }
    if contacts {
        // Personal Microsoft accounts have no organisation directory: User.ReadBasic.All isn't a valid scope for them.
        s = format!("{s} {}", if personal { CONTACTS_SCOPES_PERSONAL } else { CONTACTS_SCOPES });
    }
    s
}
#[cfg(not(test))]
const KEYRING_SERVICE: &str = "graphite-mail";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Account {
    pub email: String,
    pub display_name: String,
    /// "microsoft", "google" (Gmail through Google sign-in) or "imap" (Gmail through IMAP/SMTP with an app password)
    #[serde(default = "default_provider")]
    pub provider: String,
    /// The account has granted the extra permission that mail rules need.
    #[serde(default)]
    pub rules: bool,
    /// Microsoft: the account has granted the Contacts permissions (CONTACTS_SCOPES).
    #[serde(default)]
    pub contacts: bool,
    /// Microsoft: a personal account (outlook.com, hotmail.com, live.com) rather than an organisation's. These have no
    /// directory, and their consent is never "admin approval".
    #[serde(default)]
    pub personal: bool,
    /// IMAP accounts other than Gmail: where their servers are (Gmail accounts leave this empty).
    #[serde(default)]
    pub imap: Option<crate::imap::ImapSettings>,
}

fn default_provider() -> String {
    "microsoft".into()
}

tokio::task_local! {
    /// The account whose mailbox the current task is working on.
    static ACTIVE: String;
}

/// Runs `fut` on behalf of `email`: every access_token() call inside uses that account.
pub async fn in_account<F: std::future::Future>(email: Option<String>, fut: F) -> F::Output {
    match email {
        Some(e) => ACTIVE.scope(e, fut).await,
        None => fut.await,
    }
}

/// Email of the account the current task works for (falls back to the first account).
pub fn active_email() -> Option<String> {
    ACTIVE.try_with(|e| e.clone()).ok().or_else(|| accounts().into_iter().next().map(|a| a.email))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    /// Present because "openid" is requested; only its `tid` claim is read (to tell personal accounts apart).
    #[serde(default)]
    id_token: Option<String>,
}

/// The tenant id every personal Microsoft account (outlook.com, hotmail.com, live.com) belongs to.
pub const PERSONAL_TENANT: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// The `tid` claim of an id_token (its payload decoded, not verified: it only chooses which features to show).
pub fn tenant_of_id_token(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v["tid"].as_str().map(String::from)
}

pub fn is_personal_tenant(tid: Option<&str>) -> bool {
    tid.is_some_and(|t| t.eq_ignore_ascii_case(PERSONAL_TENANT))
}

struct Cached {
    token: String,
    expires: Instant,
}

static CACHE: Mutex<Option<std::collections::HashMap<String, Cached>>> = Mutex::new(None);

fn cache_put(email: &str, token: &str, expires_in: u64) {
    CACHE.lock().unwrap().get_or_insert_with(Default::default).insert(
        email.to_lowercase(),
        Cached { token: token.to_string(), expires: Instant::now() + Duration::from_secs(expires_in.saturating_sub(120)) },
    );
}

fn cache_get(email: &str) -> Option<String> {
    let g = CACHE.lock().unwrap();
    let c = g.as_ref()?.get(&email.to_lowercase())?;
    (Instant::now() < c.expires).then(|| c.token.clone())
}

fn rt_key(email: &str) -> String {
    format!("rt:{}", email.to_lowercase())
}

/// All signed-in accounts, oldest first. Migrates the single-account format of earlier versions.
pub fn accounts() -> Vec<Account> {
    if let Some(list) = kr_get("accounts").ok().and_then(|s| serde_json::from_str::<Vec<Account>>(&s).ok()) {
        return list;
    }
    // Earlier versions kept one account under "account" + "refresh_token".
    let legacy: Option<Account> = kr_get("account").ok().and_then(|s| serde_json::from_str(&s).ok());
    let rt = kr_get("refresh_token").ok();
    if let (Some(acc), Some(rt)) = (legacy, rt) {
        if kr_set(&rt_key(&acc.email), &rt).is_ok() && save_accounts(&[acc.clone()]).is_ok() {
            kr_delete("account");
            kr_delete("refresh_token");
        }
        return vec![acc];
    }
    vec![]
}

fn save_accounts(list: &[Account]) -> Result<()> {
    kr_set("accounts", &serde_json::to_string(list)?)
}

/// Adds or updates an account after a successful sign-in.
pub fn remember(acc: &Account, refresh_token: &str) -> Result<()> {
    kr_set(&rt_key(&acc.email), refresh_token)?;
    remember_account(acc)
}

/// Adds or updates an account in the list, without touching its sign-in secret.
pub fn remember_account(acc: &Account) -> Result<()> {
    let mut list = accounts();
    match list.iter_mut().find(|a| a.email.eq_ignore_ascii_case(&acc.email)) {
        Some(a) => {
            let (rules, contacts) = (a.rules || acc.rules, a.contacts || acc.contacts); // a permission once granted stays granted
            *a = acc.clone();
            a.rules = rules;
            a.contacts = contacts;
        }
        None => list.push(acc.clone()),
    }
    save_accounts(&list)
}

/// Records that an account no longer has the Contacts permissions (they were withdrawn).
pub fn drop_contacts_permission(email: &str) -> Result<()> {
    let mut list = accounts();
    for a in list.iter_mut().filter(|a| a.email.eq_ignore_ascii_case(email)) {
        a.contacts = false;
    }
    save_accounts(&list)
}

pub fn refresh_token_for(email: &str) -> Result<String> {
    kr_get(&rt_key(email)).context("Not signed in")
}

pub fn store_refresh_token(email: &str, rt: &str) -> Result<()> {
    kr_set(&rt_key(email), rt)
}

fn app_key(email: &str) -> String {
    format!("app:{}", email.to_lowercase())
}

/// IMAP accounts: the Google app password. It lives only in the keyring, never in a file or the database.
pub fn store_app_password(email: &str, password: &str) -> Result<()> {
    kr_set(&app_key(email), password)
}

pub fn app_password(email: &str) -> Result<String> {
    kr_get(&app_key(email))
}

pub fn forget_app_password(email: &str) {
    kr_delete(&app_key(email));
}

pub(crate) fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Whether this process runs inside the Graphite Mail snap (snapd sets SNAP_NAME for every confined app).
pub fn in_snap() -> bool {
    std::env::var("SNAP_NAME").is_ok_and(|n| n == "graphite-mail")
}

/// The one command that lets the strict snap talk to the Secret Service keyring (the interface is not connected
/// automatically unless the store grants it).
pub const SNAP_KEYRING_COMMAND: &str = "sudo snap connect graphite-mail:password-manager-service";

/// How the system keyring looks to the UI: usable, or what is wrong and what to do about it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct KeyringStatus {
    pub ok: bool,
    /// When not ok: the problem and the remedy, in plain words (never a bare D-Bus error).
    pub message: String,
    /// When not ok and one command fixes it (the snap's keyring permission): that command, for the UI to show as is.
    pub command: String,
}

impl KeyringStatus {
    pub fn fine() -> Self {
        Self { ok: true, message: String::new(), command: String::new() }
    }
}

/// Turns a keyring failure into advice. `locked`: the keyring exists but is locked or its unlock prompt was dismissed
/// (keyring's NoStorageAccess); anything else means no Secret Service could be reached at all, which in the snap
/// is almost always the unconnected `password-manager-service` interface. `detail` is the underlying error text,
/// kept at the end for bug reports.
pub fn keyring_advice(locked: bool, detail: &str, snap: bool) -> KeyringStatus {
    let detail = detail.trim();
    let tail = if detail.is_empty() { String::new() } else { format!(" ({detail})") };
    let (message, command) = if locked {
        (format!("Your system keyring is locked, or its unlock prompt was closed. Graphite Mail keeps sign-in tokens and app passwords there, so unlock it (GNOME Keyring or KWallet) and try again.{tail}"), String::new())
    } else if snap {
        (
            format!(
                "Graphite Mail can't reach your system keyring, where it keeps sign-in tokens and app passwords. \
                 The snap needs permission to use it, once: open a terminal, run the command below, then start Graphite Mail again. \
                 (If no keyring is running at all, install GNOME Keyring or KWallet first.){tail}"
            ),
            SNAP_KEYRING_COMMAND.to_string(),
        )
    } else {
        (format!("Graphite Mail can't reach a system keyring, where it keeps sign-in tokens and app passwords. Install and start GNOME Keyring or KWallet, then start Graphite Mail again.{tail}"), String::new())
    };
    KeyringStatus { ok: false, message, command }
}

/// Probes the keyring once (a read of the account list: present, or "no such entry", both mean it works) so the UI
/// can explain a missing keyring before a sign-in is attempted instead of failing at the end of one.
#[cfg(not(test))]
pub fn keyring_status() -> KeyringStatus {
    match entry("accounts").and_then(|e| e.get_password().map_err(anyhow::Error::from)) {
        Ok(_) => KeyringStatus::fine(),
        Err(e) => match e.downcast_ref::<keyring::Error>() {
            Some(keyring::Error::NoEntry) => KeyringStatus::fine(),
            Some(k) => keyring_advice(matches!(k, keyring::Error::NoStorageAccess(_)), &k.to_string(), in_snap()),
            None => keyring_advice(false, &e.to_string(), in_snap()),
        },
    }
}

/// Tests use an in-memory keyring, which is always there.
#[cfg(test)]
pub fn keyring_status() -> KeyringStatus {
    KeyringStatus::fine()
}

#[cfg(not(test))]
fn entry(key: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(KEYRING_SERVICE, key)?)
}

#[cfg(not(test))]
fn kr_get(key: &str) -> Result<String> {
    Ok(entry(key)?.get_password()?)
}

/// A failed write is what a user sees at the end of a sign-in, so it carries the advice (snap permission, no
/// keyring, locked keyring) rather than a bare "Platform secure storage failure".
#[cfg(not(test))]
fn kr_set(key: &str, value: &str) -> Result<()> {
    entry(key)?
        .set_password(value)
        .map_err(|e| anyhow!(keyring_advice(matches!(e, keyring::Error::NoStorageAccess(_)), &e.to_string(), in_snap()).message))
}

#[cfg(not(test))]
fn kr_delete(key: &str) {
    if let Ok(e) = entry(key) {
        let _ = e.delete_credential();
    }
}

/// Tests use an in-memory keyring instead of the desktop's.
#[cfg(test)]
pub(crate) static TEST_KEYRING: Mutex<Option<std::collections::HashMap<String, String>>> = Mutex::new(None);

#[cfg(test)]
fn kr_get(key: &str) -> Result<String> {
    TEST_KEYRING.lock().unwrap().get_or_insert_with(Default::default).get(key).cloned().ok_or_else(|| anyhow!("No such keyring entry"))
}

#[cfg(test)]
fn kr_set(key: &str, value: &str) -> Result<()> {
    TEST_KEYRING.lock().unwrap().get_or_insert_with(Default::default).insert(key.to_string(), value.to_string());
    Ok(())
}

#[cfg(test)]
fn kr_delete(key: &str) {
    TEST_KEYRING.lock().unwrap().get_or_insert_with(Default::default).remove(key);
}

/// Runs the interactive sign-in. `open` is called with the URL to show in the system browser.
pub async fn sign_in(open: impl FnOnce(&str) -> Result<()>) -> Result<Account> {
    sign_in_with(open, false, None).await
}

/// Sign-in that can also ask for the mail-rules permission, and pre-select an account with `hint`.
pub async fn sign_in_with(open: impl FnOnce(&str) -> Result<()>, rules: bool, hint: Option<&str>) -> Result<Account> {
    let personal = hint.and_then(|h| accounts().into_iter().find(|a| a.email.eq_ignore_ascii_case(h))).is_some_and(|a| a.personal);
    sign_in_scoped(open, rules, false, hint, personal).await
}

/// Sign-in that can ask for the rules and/or the Contacts permissions (kept once granted). `personal` is what is
/// already known about the account (false on a first sign-in): it trims the scopes and keeps the admin-approval
/// explanation away from personal accounts; the real answer comes from the token.
pub async fn sign_in_scoped(open: impl FnOnce(&str) -> Result<()>, rules: bool, contacts: bool, hint: Option<&str>, personal: bool) -> Result<Account> {
    let scope = scopes(rules, contacts, personal);
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = b64(&Sha256::digest(verifier.as_bytes()));
    let state = uuid::Uuid::new_v4().simple().to_string();

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let redirect = format!("http://localhost:{}", listener.local_addr()?.port());

    let mut url = url::Url::parse(&format!("{}/authorize", authority()))?;
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect)
        .append_pair("response_mode", "query")
        .append_pair("scope", &scope)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", if hint.is_some() { "consent" } else { "select_account" });
    if let Some(h) = hint {
        url.query_pairs_mut().append_pair("login_hint", h);
    }
    open(url.as_str())?;

    let explain = |e: anyhow::Error| if personal { e } else { mark_admin_approval(e) };
    let code = wait_for_sign_in(&listener, &state).await.map_err(explain)?;

    let tok = token_request(&[
        ("client_id", CLIENT_ID),
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect),
        ("code_verifier", &verifier),
        ("scope", &scope),
    ])
    .await
    .map_err(explain)?;

    let mut account = fetch_me(&tok.access_token).await?;
    account.rules = rules;
    account.contacts = contacts;
    account.personal = personal || is_personal_tenant(tok.id_token.as_deref().and_then(|t| tenant_of_id_token(t)).as_deref());
    remember(&account, tok.refresh_token.as_deref().ok_or(anyhow!("No refresh token"))?)?;
    cache_put(&account.email, &tok.access_token, tok.expires_in);
    Ok(account)
}

/// Lets the user give up on a browser sign-in that never comes back.
static SIGN_IN_CANCEL: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Stops every browser sign-in that is still waiting (Microsoft or Google).
pub fn cancel_sign_in() {
    SIGN_IN_CANCEL.notify_waiters();
}

/// Waits for the browser to come back with the sign-in code: at most five minutes, or until cancel_sign_in().
pub(crate) async fn wait_for_sign_in(listener: &TcpListener, state: &str) -> Result<String> {
    tokio::select! {
        r = tokio::time::timeout(Duration::from_secs(300), wait_for_code(listener, state)) => r.map_err(|_| anyhow!("Sign-in timed out"))?,
        _ = SIGN_IN_CANCEL.notified() => Err(anyhow!("Sign-in was cancelled.")),
    }
}

pub(crate) async fn wait_for_code(listener: &TcpListener, state: &str) -> Result<String> {
    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await?;
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req.split_whitespace().nth(1).unwrap_or("/");
        let u = url::Url::parse(&format!("http://localhost{path}"))?;
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        if q.is_empty() {
            continue; // favicon etc.
        }
        let (ok, body) = match (q.get("code"), q.get("error")) {
            (Some(c), _) if q.get("state").map(|s| s == state).unwrap_or(false) => (
                Some(c.clone()),
                "Signed in. You can close this tab and return to Graphite Mail.",
            ),
            (_, Some(_)) => (
                None,
                "Sign-in was cancelled or failed. Return to Graphite Mail.",
            ),
            _ => (None, "Invalid response."),
        };
        let html = format!(
            "<html><body style='font-family:sans-serif;padding:3em'><h2>Graphite Mail</h2><p>{body}</p></body></html>"
        );
        let _ = sock
            .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len()).as_bytes())
            .await;
        return ok.ok_or_else(|| {
            anyhow!(
                q.get("error_description")
                    .cloned()
                    .unwrap_or_else(|| body.to_string())
            )
        });
    }
}

async fn token_request(form: &[(&str, &str)]) -> Result<TokenResponse> {
    let resp = crate::http::client()
        .post(format!("{}/token", authority()))
        .form(form)
        .send()
        .await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        return Err(match v["error"].as_str() {
            Some("invalid_grant" | "interaction_required" | "consent_required") => {
                anyhow!("Your Microsoft sign-in has expired. Please sign out and sign in again.")
            }
            _ => anyhow!(
                "Microsoft sign-in failed: {}",
                v["error_description"]
                    .as_str()
                    .and_then(|d| d.lines().next())
                    .unwrap_or("unknown error")
            ),
        });
    }
    let tok: TokenResponse = resp.json().await?;
    Ok(tok)
}

async fn fetch_me(token: &str) -> Result<Account> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Me {
        display_name: Option<String>,
        mail: Option<String>,
        user_principal_name: String,
    }
    let me: Me = crate::http::client()
        .get(format!("{}/me", graph()))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(Account {
        email: me.mail.unwrap_or(me.user_principal_name.clone()),
        display_name: me.display_name.unwrap_or(me.user_principal_name),
        provider: "microsoft".into(),
        rules: false,
        contacts: false,
        personal: false,
        imap: None,
    })
}

/// Returns a valid access token for the active account, refreshing silently with its stored refresh token.
pub async fn access_token() -> Result<String> {
    #[cfg(test)]
    if let Ok(t) = std::env::var("GM_TEST_TOKEN") {
        return Ok(t);
    }
    let email = active_email().ok_or(anyhow!("Not signed in"))?;
    if let Some(t) = cache_get(&email) {
        return Ok(t);
    }
    let acc = accounts().into_iter().find(|a| a.email.eq_ignore_ascii_case(&email));
    match acc.as_ref().map(|a| a.provider.as_str()) {
        Some("google") => return crate::google::access_token(&email).await,
        // IMAP accounts sign in to the mail server with their app password; they have no OAuth token.
        Some(crate::imap::PROVIDER) => return Err(anyhow!("{email} uses an app password, not a sign-in token")),
        _ => {}
    }
    let rt = refresh_token_for(&email)?;
    let (rules, contacts, personal) = acc.as_ref().map_or((false, false, false), |a| (a.rules, a.contacts, a.personal));
    let refresh = |scope: String| {
        let rt = rt.clone();
        async move { token_request(&[("client_id", CLIENT_ID), ("grant_type", "refresh_token"), ("refresh_token", &rt), ("scope", &scope)]).await }
    };
    let tok = match refresh(scopes(rules, contacts, personal)).await {
        Ok(t) => t,
        // The Contacts permissions were withdrawn (by the user or an administrator): mail and calendar must keep
        // working, so sign in without them and let the Contacts view ask again.
        Err(_) if contacts => {
            let t = refresh(scopes(rules, false, personal)).await.map_err(|e| anyhow!("{email}: {e}"))?;
            let _ = drop_contacts_permission(&email);
            t
        }
        Err(e) => return Err(anyhow!("{email}: {e}")),
    };
    if let Some(new_rt) = tok.refresh_token {
        store_refresh_token(&email, &new_rt)?;
    }
    cache_put(&email, &tok.access_token, tok.expires_in);
    Ok(tok.access_token)
}

/// Cache a token obtained elsewhere (Google sign-in/refresh).
pub fn cache_token(email: &str, token: &str, expires_in: u64) {
    cache_put(email, token, expires_in);
}

pub fn current_account() -> Option<Account> {
    accounts().into_iter().next()
}

/// Signs out one account (or all when `email` is None) and forgets its keyring entries.
pub fn sign_out_account(email: Option<&str>) -> Result<()> {
    let list = accounts();
    let (gone, keep): (Vec<_>, Vec<_>) = list.into_iter().partition(|a| email.map_or(true, |e| a.email.eq_ignore_ascii_case(e)));
    for a in &gone {
        kr_delete(&rt_key(&a.email));
        kr_delete(&app_key(&a.email));
        if let Some(c) = CACHE.lock().unwrap().as_mut() {
            c.remove(&a.email.to_lowercase());
        }
    }
    if keep.is_empty() {
        kr_delete("accounts");
    } else {
        save_accounts(&keep)?;
    }
    Ok(())
}

pub fn sign_out() -> Result<()> {
    sign_out_account(None)
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    #[test]
    fn an_unreachable_keyring_is_explained_with_the_snap_command_or_the_keyring_to_install() {
        let dbus = "Platform secure storage failure: org.freedesktop.DBus.Error.AccessDenied";
        let snap = super::keyring_advice(false, dbus, true);
        assert!(!snap.ok);
        assert_eq!(snap.command, "sudo snap connect graphite-mail:password-manager-service");
        assert!(snap.message.contains("can't reach your system keyring") && snap.message.contains("run the command below"), "{}", snap.message);
        assert!(snap.message.ends_with(&format!("({dbus})")), "the raw error stays at the end for bug reports: {}", snap.message);

        let deb = super::keyring_advice(false, dbus, false);
        assert!(!deb.ok && deb.command.is_empty());
        assert!(deb.message.contains("GNOME Keyring or KWallet") && !deb.message.contains("snap"), "{}", deb.message);

        let locked = super::keyring_advice(true, "", true);
        assert!(locked.message.starts_with("Your system keyring is locked") && locked.command.is_empty() && !locked.message.contains("()"), "{}", locked.message);

        assert!(super::keyring_status().ok, "the test keyring is always there");
        assert_eq!(super::KeyringStatus::fine(), super::KeyringStatus { ok: true, message: String::new(), command: String::new() });
        assert!(!super::in_snap(), "tests don't run inside the snap");
    }

    #[test]
    fn admin_approval_is_recognised_from_microsofts_error_texts() {
        for t in [
            "AADSTS90094: The grant requires admin permission.",
            "aadsts90095: Admin consent is required for the permissions requested.",
            "AADSTS65001: The user or administrator has not consented to use the application with ID '4fd8…'. Send an interactive authorization request for this user and resource.",
            "admin_consent_required",
            "Need admin approval",
        ] {
            assert!(super::admin_approval_needed(t), "{t}");
        }
        for t in ["AADSTS65004: User declined to consent to access the app.", "AADSTS50058: A silent sign-in request was sent but no user is signed in.", "Sign-in was cancelled.", "invalid_grant"] {
            assert!(!super::admin_approval_needed(t), "{t}");
        }
        let e = super::mark_admin_approval(anyhow::anyhow!("Microsoft sign-in failed: AADSTS90094: The grant requires admin permission."));
        assert!(e.to_string().starts_with(super::ADMIN_APPROVAL_MARK) && e.to_string().contains("IT admin"));
        assert!(!e.to_string().contains("AADSTS"), "the user sees words, not codes");
        assert_eq!(super::mark_admin_approval(anyhow::anyhow!("Sign-in was cancelled.")).to_string(), "Sign-in was cancelled.");
    }

    #[test]
    fn the_admin_consent_link_names_the_tenant_the_app_and_the_permissions_but_no_secret() {
        let u = super::admin_consent_url(Some("Ann@Contoso.com"));
        assert!(u.starts_with("https://login.microsoftonline.com/contoso.com/v2.0/adminconsent?"), "{u}");
        assert!(u.contains(&format!("client_id={}", super::CLIENT_ID)));
        for p in ["Mail.ReadWrite", "Mail.Send", "Calendars.ReadWrite", "MailboxSettings.ReadWrite", "Contacts.Read", "People.Read", "User.ReadBasic.All"] {
            assert!(u.contains(p), "{p} in {u}");
        }
        assert!(!u.to_lowercase().contains("secret") && !u.contains("openid"));
        assert!(super::admin_consent_url(None).contains("/organizations/"));
        assert!(super::admin_consent_url(Some("me@outlook.com")).contains("/organizations/"), "a personal domain is no tenant");
    }

    #[test]
    fn personal_accounts_are_told_apart_by_the_token_tenant_and_get_no_directory_scope() {
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"tid":"9188040d-6c67-4c5b-b112-36a304b66dad","name":"Ann"}"#);
        let tok = format!("eyJhbGciOiJub25lIn0.{claims}.sig");
        assert!(super::is_personal_tenant(super::tenant_of_id_token(&tok).as_deref()));
        let org = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"tid":"72f988bf-86f1-41af-91ab-2d7cd011db47"}"#);
        assert!(!super::is_personal_tenant(super::tenant_of_id_token(&format!("h.{org}.s")).as_deref()));
        assert!(super::tenant_of_id_token("not a token").is_none() && !super::is_personal_tenant(None));
        assert!(!super::scopes(true, true, true).contains("User.ReadBasic.All") && super::scopes(true, true, true).contains("People.Read"));
        assert!(super::scopes(true, true, false).contains("User.ReadBasic.All"));
    }

    #[tokio::test]
    async fn a_waiting_browser_sign_in_can_be_cancelled() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let waiting = tokio::spawn(async move { super::wait_for_sign_in(&listener, "state").await });
        // Let it start waiting, then give up as the Cancel button (or the UI's time limit) does.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        super::cancel_sign_in();
        let r = tokio::time::timeout(std::time::Duration::from_secs(2), waiting).await.expect("stops at once").unwrap();
        assert!(r.unwrap_err().to_string().contains("cancelled"));
    }
}
