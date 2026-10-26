//! Personal Microsoft accounts (outlook.com, hotmail.com, live.com) and admin approval, against a pretend identity
//! service and a pretend Microsoft Graph on 127.0.0.1.

use crate::{auth, contacts, storage};
use base64::Engine;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// (method, path+query, body) of every request.
type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

/// A pretend server: `route(method, path, body)` answers with JSON; every request is recorded.
async fn fake(route: impl Fn(&str, &str, &str) -> (u16, Value) + Send + Sync + 'static) -> (String, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::new(Mutex::new(vec![]));
    let log = seen.clone();
    let route = Arc::new(route);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let (log, route) = (log.clone(), route.clone());
            tokio::spawn(async move {
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
                let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push((method.clone(), path.clone(), body.clone()));
                let (st, v) = route(&method, &path, &body);
                let out = v.to_string();
                let resp = format!("HTTP/1.1 {st} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len());
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    (base, seen)
}

/// An id_token whose payload carries `tid` (the signature is never checked: only the claim is read).
fn id_token(tid: &str) -> String {
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({ "tid": tid, "name": "Ann" }).to_string());
    format!("eyJhbGciOiJub25lIn0.{claims}.sig")
}

/// The "browser": reads redirect_uri and state from the sign-in URL and comes back with `answer` (query parameters).
fn browser(answer: &'static str) -> impl FnOnce(&str) -> anyhow::Result<()> {
    move |url: &str| {
        let u = url::Url::parse(url)?;
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        let redirect = q["redirect_uri"].clone();
        let state = q["state"].clone();
        tokio::spawn(async move {
            let r = url::Url::parse(&redirect).unwrap();
            let addr = format!("{}:{}", r.host_str().unwrap(), r.port().unwrap());
            if let Ok(mut s) = tokio::net::TcpStream::connect(addr).await {
                let _ = s.write_all(format!("GET /?state={state}&{answer} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).await;
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf).await;
            }
        });
        Ok(())
    }
}

fn setup() {
    let tmp = std::env::temp_dir().join(format!("gm-personal-{}", uuid::Uuid::new_v4()));
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", tmp.join("cfg"));
        std::env::remove_var("GM_TEST_TOKEN");
    }
    storage::initialize(tmp.join("vault").to_str().unwrap()).unwrap();
    let _ = auth::sign_out();
}

#[tokio::test]
async fn a_personal_account_is_recognised_gets_no_directory_scope_and_no_directory_sync() {
    let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    setup();
    let (base, seen) = fake(|m, p, _| match (m, p.split('?').next().unwrap()) {
        ("POST", "/token") => (200, json!({ "access_token": "t-1", "refresh_token": "r-1", "expires_in": 3600, "id_token": id_token(auth::PERSONAL_TENANT) })),
        ("GET", "/me") => (200, json!({ "displayName": "Ann Example", "userPrincipalName": "ann@outlook.com", "mail": "ann@outlook.com" })),
        ("GET", x) if x.starts_with("/me/contactFolders") || x.starts_with("/me/contacts") || x.starts_with("/me/people") => (200, json!({ "value": [] })),
        ("GET", x) if x.starts_with("/users") => (400, json!({ "error": { "code": "Request_UnsupportedQuery", "message": "not for personal accounts" } })),
        _ => (404, json!({ "error": { "message": "no route" } })),
    })
    .await;
    unsafe {
        std::env::set_var("GM_TEST_AUTHORITY", &base);
        std::env::set_var("GM_TEST_GRAPH", &base);
    }

    // First sign-in: nothing is known yet; the token's tenant says it is a personal account.
    let acc = auth::sign_in_scoped(browser("code=c-1"), false, false, None, false).await.unwrap();
    assert_eq!((acc.email.as_str(), acc.display_name.as_str(), acc.personal), ("ann@outlook.com", "Ann Example", true));
    assert!(auth::accounts()[0].personal, "remembered as personal");
    let token_scope = |i: usize| seen.lock().unwrap().iter().filter(|(_, p, _)| p == "/token").nth(i).map(|(_, _, b)| b.clone()).unwrap();
    assert!(token_scope(0).contains("Mail.ReadWrite") && !token_scope(0).contains("User.ReadBasic.All"));

    // Granting Contacts for a personal account asks for contacts and people, never the directory.
    let again = auth::sign_in_scoped(browser("code=c-2"), false, true, Some("ann@outlook.com"), acc.personal).await.unwrap();
    assert!(again.personal && again.contacts);
    let s = token_scope(1);
    assert!(s.contains("Contacts.Read") && s.contains("People.Read") && !s.contains("User.ReadBasic.All"), "{s}");

    // Contacts sync: personal contacts yes, /users never; the status says the directory doesn't apply.
    let id = crate::sync::ensure_account_pub(&again).unwrap();
    auth::in_account(Some("ann@outlook.com".into()), contacts::sync_account(&id, true)).await.unwrap();
    assert!(!seen.lock().unwrap().iter().any(|(_, p, _)| p.starts_with("/users")), "no directory request for a personal account");
    assert!(seen.lock().unwrap().iter().any(|(_, p, _)| p.starts_with("/me/contact")), "personal contacts are synced");
    let st = contacts::statuses(Some("ann@outlook.com")).unwrap();
    assert_eq!((st[0].personal_account, st[0].directory.as_str(), st[0].personal_count), (true, "n/a", 0));
    assert_eq!(contacts::overview("", None).unwrap().accounts[0].directory, "n/a");
    unsafe {
        std::env::remove_var("GM_TEST_AUTHORITY");
        std::env::remove_var("GM_TEST_GRAPH");
    }
}

#[tokio::test]
async fn admin_approval_is_explained_for_organisations_and_never_for_personal_accounts() {
    let _g = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    setup();
    let (base, _) = fake(|_, _, _| (500, json!({}))).await;
    unsafe { std::env::set_var("GM_TEST_AUTHORITY", &base) };
    let answer = "error=admin_consent_required&error_description=AADSTS90094%3A+The+grant+requires+admin+permission.";
    let e = auth::sign_in_scoped(browser(answer), false, false, None, false).await.unwrap_err().to_string();
    assert!(e.starts_with(auth::ADMIN_APPROVAL_MARK) && e.contains("administrator") && !e.contains("AADSTS"), "{e}");
    let e = auth::sign_in_scoped(browser(answer), false, true, Some("ann@outlook.com"), true).await.unwrap_err().to_string();
    assert!(!e.starts_with(auth::ADMIN_APPROVAL_MARK), "personal accounts get the plain error: {e}");
    // Any other failure is passed through as it was.
    let e = auth::sign_in_scoped(browser("error=access_denied&error_description=AADSTS65004%3A+User+declined+to+consent."), false, false, None, false).await.unwrap_err().to_string();
    assert!(e.contains("AADSTS65004") && !e.starts_with(auth::ADMIN_APPROVAL_MARK));
    unsafe { std::env::remove_var("GM_TEST_AUTHORITY") };
}
