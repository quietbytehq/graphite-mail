//! Checks that keep the security fixes from being undone later (they read the repository files).

use std::path::{Path, PathBuf};

#[path = "../build_support.rs"]
mod build_support;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if matches!(name.as_str(), "target" | "node_modules" | ".git" | "gen" | "dist" | "icons") {
            continue;
        }
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn build_settings_prefer_environment_then_the_untracked_file() {
    let file = r#"{ "client_id": "file-id", "client_secret": "file-secret" }"#;
    assert_eq!(build_support::pick(Some("env".into()), Some(file), "client_secret").as_deref(), Some("env"));
    assert_eq!(build_support::pick(None, Some(file), "client_secret").as_deref(), Some("file-secret"));
    assert_eq!(build_support::pick(None, Some(file), "client_id").as_deref(), Some("file-id"));
    assert_eq!(build_support::pick(Some("  ".into()), Some(file), "client_id").as_deref(), Some("file-id"), "blank env is ignored");
    assert_eq!(build_support::pick(None, None, "client_secret"), None);
    assert_eq!(build_support::pick(None, Some("{}"), "client_secret"), None);
    // anything that could break out of the build setting is refused
    assert_eq!(build_support::pick(Some("a\nb".into()), None, "client_secret"), None);
    assert_eq!(build_support::pick(Some("a\"b".into()), None, "client_secret"), None);
    assert_eq!(build_support::pick(None, Some(r#"{"client_secret": "x\ny"}"#), "client_secret"), None);
}

#[test]
fn the_secret_file_is_ignored_by_git_and_an_example_exists() {
    let ignore = std::fs::read_to_string(root().join("../.gitignore")).unwrap();
    assert!(ignore.lines().any(|l| l.trim() == "src-tauri/google_oauth.json"), ".gitignore must list google_oauth.json");
    let example = std::fs::read_to_string(root().join("google_oauth.example.json")).unwrap();
    assert!(example.contains("YOUR-CLIENT-SECRET") && !example.contains("GOCSPX"));
}

#[test]
fn no_google_client_secret_in_tracked_sources() {
    let mut files = vec![];
    for d in ["src", "../src", "../docs", "../public", "capabilities"] {
        walk(&root().join(d), &mut files);
    }
    for f in ["build.rs", "build_support.rs", "Cargo.toml", "tauri.conf.json", "../README.md", "../PLAN.md", "../package.json"] {
        files.push(root().join(f));
    }
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue }; // binary files are skipped
        for (i, _) in text.match_indices("GOCSPX-") {
            let tail: String = text[i + 7..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
            assert!(tail.len() < 12, "{} looks like a Google client secret", f.display());
        }
    }
}

fn json_file(rel: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(root().join(rel)).unwrap()).unwrap()
}

fn directives(csp: &str) -> std::collections::BTreeMap<String, Vec<String>> {
    csp.split(';')
        .filter_map(|d| {
            let mut parts = d.split_whitespace();
            Some((parts.next()?.to_string(), parts.map(String::from).collect()))
        })
        .collect()
}

#[test]
fn the_app_has_a_strict_content_security_policy() {
    let conf = json_file("tauri.conf.json");
    let csp = conf["app"]["security"]["csp"].as_str().expect("a CSP must be set (not null)");
    let d = directives(csp);
    let want = |name: &str, values: &[&str]| assert_eq!(d.get(name).map(|v| v.iter().map(String::as_str).collect::<Vec<_>>()), Some(values.to_vec()), "{name}");
    want("default-src", &["'self'"]);
    want("img-src", &["'self'", "data:", "blob:", "https:"]);
    want("style-src", &["'self'", "'unsafe-inline'"]);
    want("frame-src", &["'self'"]);
    want("connect-src", &["ipc:", "http://ipc.localhost"]);
    want("object-src", &["'none'"]);
    // scripts come only from the app itself: no inline, no eval, no remote
    assert!(!d.contains_key("script-src"), "script-src falls back to default-src 'self'");
    for bad in ["'unsafe-eval'", "*", "http:", "ws:", "wss:", "'unsafe-hashes'"] {
        assert!(!d.values().flatten().any(|v| v == bad), "CSP must not contain {bad}");
    }
    assert!(!d.get("default-src").unwrap().iter().any(|v| v == "'unsafe-inline'"));
    // the development-only policy must not be what ships
    assert_ne!(conf["app"]["security"]["csp"], conf["app"]["security"]["devCsp"]);
}

fn frontend_sources() -> Vec<(PathBuf, String)> {
    let mut files = vec![];
    walk(&root().join("../src"), &mut files);
    files.into_iter().filter(|f| f.extension().is_some_and(|e| e == "tsx" || e == "ts")).map(|f| { let t = std::fs::read_to_string(&f).unwrap(); (f, t) }).collect()
}

/// The permissions the web code really needs, worked out from what it imports and calls.
fn needed_permissions() -> std::collections::BTreeSet<String> {
    let mut need = std::collections::BTreeSet::new();
    for (_, t) in frontend_sources() {
        if t.contains("@tauri-apps/api/event") {
            need.insert("core:event:allow-listen".to_string());
            need.insert("core:event:allow-unlisten".to_string());
        }
        for (call, perm) in [(".minimize(", "core:window:allow-minimize"), (".toggleMaximize(", "core:window:allow-toggle-maximize"), (".close()", "core:window:allow-close"), ("data-tauri-drag-region", "core:window:allow-start-dragging")] {
            if t.contains(call) && (t.contains("getCurrentWindow") || call == "data-tauri-drag-region") {
                need.insert(perm.to_string());
            }
        }
        for line in t.lines().filter(|l| l.contains("from \"@tauri-apps/plugin-dialog\"")) {
            let names = line.split('{').nth(1).and_then(|x| x.split('}').next()).unwrap_or_default();
            for n in names.split(',').map(|n| n.trim().split_whitespace().next().unwrap_or_default()) {
                if matches!(n, "ask" | "open" | "save" | "confirm" | "message") {
                    need.insert(format!("dialog:allow-{n}"));
                }
            }
        }
    }
    need
}

#[test]
fn window_permissions_are_exactly_what_the_ui_uses() {
    let cap = json_file("capabilities/default.json");
    let granted: std::collections::BTreeSet<String> = cap["permissions"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect();
    assert_eq!(granted, needed_permissions(), "grant exactly what the web code uses, nothing more");
    assert_eq!(cap["windows"], serde_json::json!(["main"]), "only the main window gets IPC access (the splash window runs no code)");
    for p in &granted {
        assert!(!p.ends_with(":default"), "{p}: broad default permission sets are not allowed");
        assert!(!p.starts_with("opener:") && !p.starts_with("notification:") && !p.contains("reveal"), "{p}");
        assert!(!p.contains("deny-"), "{p}: no conflicting allow/deny pairs");
    }
    // links and notifications are handled in Rust, so the web code must not import those plugins
    for (f, t) in frontend_sources() {
        for plugin in ["@tauri-apps/plugin-opener", "@tauri-apps/plugin-notification", "@tauri-apps/plugin-fs", "@tauri-apps/plugin-shell"] {
            assert!(!t.contains(plugin), "{} imports {plugin}, which would need a permission", f.display());
        }
    }
}

#[test]
fn only_the_main_window_has_a_capability_file() {
    let mut files = vec![];
    walk(&root().join("capabilities"), &mut files);
    assert_eq!(files.len(), 1, "one capability file: {files:?}");
}

#[test]
fn the_email_frame_can_only_ask_for_popups_which_the_app_turns_into_checked_links() {
    let html = std::fs::read_to_string(root().join("../src/emailHtml.ts")).unwrap();
    let sandbox = html.split("export const FRAME_SANDBOX = \"").nth(1).and_then(|r| r.split('"').next()).expect("FRAME_SANDBOX");
    let flags: Vec<&str> = sandbox.split_whitespace().collect();
    assert_eq!(flags, vec!["allow-popups"], "no scripts, same-origin, forms, top navigation or popup escape in emails");
    assert!(html.contains("node.setAttribute(\"target\", \"_blank\")"), "links ask for a new window");
    assert!(html.contains("purify.sanitize("), "email HTML goes through DOMPurify");
    let frame = std::fs::read_to_string(root().join("../src/MessageFrame.tsx")).unwrap();
    assert!(frame.contains("sandbox={FRAME_SANDBOX}"));
    // Every frame showing untrusted HTML is the MessageFrame; nothing else makes an iframe.
    for (f, t) in frontend_sources() {
        if !f.ends_with("MessageFrame.tsx") && !f.to_string_lossy().contains(".test.") {
            assert!(!t.contains("<iframe"), "{}: use MessageFrame", f.display());
        }
    }
    let lib = std::fs::read_to_string(root().join("src/lib.rs")).unwrap();
    assert!(lib.contains(".on_new_window(") && lib.contains("NewWindowResponse::Deny"), "the app must refuse every new window");
    // Both windows (main and splash) have a navigation and a new-window handler.
    assert_eq!(lib.matches(".on_navigation(").count(), 2, "every window gets a navigation handler");
    assert_eq!(lib.matches(".on_new_window(").count(), 2, "every window refuses new windows");
    assert_eq!(lib.matches("WebviewWindowBuilder::").count(), 2, "a new window needs the same handlers");
}

/// Email HTML never reaches the app's own DOM as an HTML string: no dangerouslySetInnerHTML at all, and the only
/// innerHTML/insertAdjacentHTML writes are the composer's own markup (blank lines and the escaped signature).
#[test]
fn email_html_is_never_inserted_with_inner_html() {
    let allowed = [
        "body.current.innerHTML = `<p><br></p>${sig}`; // our own markup; the signature text is escaped",
        "holder.innerHTML = esc((await invoke<string>(\"get_signature\", { email }).catch(() => \"\")).trim());",
        "if (sig) body.current.insertAdjacentHTML(\"afterbegin\", `<p><br></p>${sig}`);",
    ];
    for (f, t) in frontend_sources() {
        if f.to_string_lossy().contains(".test.") {
            continue;
        }
        assert!(!t.contains("dangerouslySetInnerHTML"), "{}", f.display());
        for line in t.lines().map(str::trim) {
            let writes = line.contains("insertAdjacentHTML") || line.contains("outerHTML =") || line.split("innerHTML").skip(1).any(|rest| rest.trim_start().starts_with('=') && !rest.trim_start().starts_with("=="));
            if writes {
                assert!(allowed.contains(&line), "{}: HTML string written into the page: {line}", f.display());
            }
        }
    }
}

#[test]
fn the_app_password_account_type_needs_no_google_client() {
    // IMAP/SMTP accounts must build and run without GRAPHITE_GOOGLE_CLIENT_SECRET: no OAuth code in that path.
    let src = std::fs::read_to_string(root().join("src/imap.rs")).unwrap();
    for banned in ["CLIENT_SECRET", "CLIENT_ID", "client()", "access_token", "sign_in", "oauth2", "GRAPHITE_GOOGLE"] {
        assert!(!src.contains(banned), "imap.rs must not use {banned}");
    }
}

#[test]
fn app_passwords_stay_out_of_logs_and_files() {
    let src = std::fs::read_to_string(root().join("src/imap.rs")).unwrap();
    // Every log line in the IMAP code prints only an error that was already redacted or carries no password.
    for line in src.lines().filter(|l| l.contains("eprintln!") || l.contains("println!") || l.contains("dbg!")) {
        assert!(!line.contains("pw") && !line.contains("password") && !line.contains("expose"), "suspicious log line: {line}");
    }
    // The password is read from the keyring at the moment it is needed, and only handed to the two servers.
    let uses: Vec<&str> = src.lines().filter(|l| l.contains(".expose()")).map(str::trim).collect();
    assert!(uses.len() <= 6, "the app password is used in more places than expected: {uses:#?}");
    let auth = std::fs::read_to_string(root().join("src/auth.rs")).unwrap();
    assert!(auth.contains("fn store_app_password") && auth.contains("kr_set(&app_key(email), password)"), "app passwords go to the keyring");
    for f in ["src/storage.rs", "src/notify.rs", "src/drafts.rs"] {
        let text = std::fs::read_to_string(root().join(f)).unwrap();
        assert!(!text.contains("app_password"), "{f} must not store app passwords");
    }
}

#[test]
fn contacts_stay_local_and_out_of_logs() {
    let src = std::fs::read_to_string(root().join("src/contacts.rs")).unwrap();
    // Log lines may name the user's own account and a status, never a contact or a response body.
    for line in src.lines().filter(|l| l.contains("eprintln!") || l.contains("println!") || l.contains("dbg!")) {
        for banned in ["name", "mail\"", "emails", "phone", "body", "page", "value", "url", "token"] {
            assert!(!line.contains(banned), "a log line may expose contact data: {line}");
        }
    }
    // Graph errors keep only the HTTP status, not the text (which can quote contact data).
    assert!(src.contains("struct Failed(u16)"));
    // The only place contacts go is the local database (no other network calls than Graph reads).
    assert!(!src.contains(".post(") && !src.contains(".put(") && !src.contains(".patch("), "contacts are only ever read from Graph");
    // Contacts go when their account goes.
    let schema = std::fs::read_to_string(root().join("src/storage.rs")).unwrap();
    let table = &schema[schema.find("CREATE TABLE IF NOT EXISTS contacts").unwrap()..];
    assert!(table[..table.find(");").unwrap()].contains("REFERENCES accounts(id) ON DELETE CASCADE"));
}
