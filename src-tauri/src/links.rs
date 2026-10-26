//! The one place that decides what a link may do. Only http, https and mailto are ever acted on;
//! everything else (javascript:, data:, file:, ftp:, blob:, custom schemes …) is refused.

use url::Url;

/// A link longer than this is refused (nothing legitimate needs more, and it keeps argument lists sane).
const MAX_LEN: usize = 8192;

#[derive(Debug, PartialEq)]
pub enum Action {
    /// The app's own pages (and the email frame loading itself): let the web view load them.
    Allow,
    /// A web link: open it in the system browser.
    OpenExternal(String),
    /// A mailto link: start a new message to this address.
    Compose(String),
    /// Anything else: do nothing.
    Deny,
}

/// What to do with a navigation request. `dev` allows the local development server.
pub fn classify(url: &Url, dev: bool) -> Action {
    if url.as_str().len() > MAX_LEN {
        return Action::Deny;
    }
    match url.scheme() {
        // The app's own pages (tauri://localhost on Linux, http://tauri.localhost on Windows) and the (srcdoc) email
        // frame. Nothing else is ever loaded into the window: not another host on the app's scheme, not ipc:/asset:.
        "tauri" => if url.host_str() == Some("localhost") { Action::Allow } else { Action::Deny },
        "about" => if matches!(url.path(), "srcdoc" | "blank") { Action::Allow } else { Action::Deny },
        "http" | "https" => {
            let host = url.host_str().unwrap_or_default();
            if host == "tauri.localhost" || (dev && matches!(host, "localhost" | "127.0.0.1")) {
                return Action::Allow;
            }
            match external(url.as_str()) {
                Ok(u) => Action::OpenExternal(u.to_string()),
                Err(_) => Action::Deny,
            }
        }
        "mailto" => mailto_address(url).map(Action::Compose).unwrap_or(Action::Deny),
        _ => Action::Deny,
    }
}

/// A web address that may be opened in the browser: http or https, with a host, not absurdly long.
pub fn external(raw: &str) -> Result<Url, String> {
    if raw.len() > MAX_LEN {
        return Err("That link is too long.".into());
    }
    let u = Url::parse(raw.trim()).map_err(|_| "That isn't a valid web address.".to_string())?;
    match u.scheme() {
        "http" | "https" if u.host_str().is_some_and(|h| !h.is_empty()) => Ok(u),
        "http" | "https" => Err("That web address has no site name.".into()),
        other => Err(format!("Only web (http/https) and email (mailto) links can be opened, not “{other}:”.")),
    }
}

/// The address of a mailto link ("mailto:a@b.c?subject=x" → "a@b.c"), if it is a plausible single address.
pub fn mailto_address(url: &Url) -> Option<String> {
    if url.scheme() != "mailto" {
        return None;
    }
    let first = url.path().split(',').next().unwrap_or_default();
    let addr = percent_decode(first)?;
    let ok = !addr.is_empty()
        && addr.len() <= 320
        && addr.matches('@').count() == 1
        && !addr.starts_with('@')
        && !addr.ends_with('@')
        && addr.chars().all(|c| !c.is_control() && !c.is_whitespace() && !matches!(c, '<' | '>' | '"' | '\'' | '\\' | ';'));
    ok.then_some(addr)
}

fn percent_decode(s: &str) -> Option<String> {
    percent_encoding::percent_decode_str(s).decode_utf8().ok().map(|c| c.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the app does with a link; text that isn't even a valid address is refused.
    fn act(s: &str) -> Action {
        Url::parse(s).map(|u| classify(&u, false)).unwrap_or(Action::Deny)
    }

    #[test]
    fn web_and_mail_links_are_allowed() {
        assert_eq!(act("https://example.com/a?b=1#c"), Action::OpenExternal("https://example.com/a?b=1#c".into()));
        assert_eq!(act("http://example.com"), Action::OpenExternal("http://example.com/".into()));
        assert_eq!(act("HTTPS://EXAMPLE.COM/x"), Action::OpenExternal("https://example.com/x".into()), "scheme and host are normalised");
        assert_eq!(act("mailto:boss@example.com"), Action::Compose("boss@example.com".into()));
        assert_eq!(act("mailto:boss@example.com?subject=Hi%20there"), Action::Compose("boss@example.com".into()));
        assert_eq!(act("mailto:a%40b.com"), Action::Compose("a@b.com".into()));
    }

    #[test]
    fn dangerous_schemes_are_refused() {
        for u in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(document.cookie)",
            "data:text/html,<script>alert(1)</script>",
            "data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==",
            "file:///etc/passwd",
            "file://localhost/home/user/.ssh/id_rsa",
            "ftp://example.com/file",
            "vbscript:msgbox(1)",
            "blob:https://example.com/123",
            "view-source:https://example.com",
            "ssh://host",
            "smb://host/share",
            "tel:+123456",
            "sms:+123456",
            "intent://x#Intent;end",
            "chrome://settings",
            "steam://run/1",
            "x-scheme-handler/evil:thing",
        ] {
            assert_eq!(act(u), Action::Deny, "{u}");
            assert!(external(u).is_err(), "{u}");
        }
    }

    #[test]
    fn app_pages_are_loaded_but_other_about_pages_are_not() {
        assert_eq!(act("tauri://localhost/index.html"), Action::Allow);
        // the window never navigates to another host on the app's scheme, or to the IPC/asset schemes
        assert_eq!(act("tauri://evil.example/index.html"), Action::Deny);
        assert_eq!(act("ipc://localhost/cmd"), Action::Deny);
        assert_eq!(act("asset://localhost/etc/passwd"), Action::Deny);
        assert!(matches!(act("http://ipc.localhost/cmd"), Action::OpenExternal(_)), "not loaded in the app");
        assert!(matches!(act("http://tauri.localhost.evil.example/"), Action::OpenExternal(_)), "a look-alike host is just a web link");
        assert_eq!(act("about:srcdoc"), Action::Allow);
        assert_eq!(act("about:blank"), Action::Allow);
        assert_eq!(act("about:config"), Action::Deny);
        assert_eq!(act("http://tauri.localhost/"), Action::Allow);
        // the dev server is only trusted in development builds
        assert!(matches!(act("http://localhost:1420/"), Action::OpenExternal(_)));
        assert_eq!(classify(&Url::parse("http://localhost:1420/").unwrap(), true), Action::Allow);
        assert_eq!(classify(&Url::parse("http://127.0.0.1:1420/").unwrap(), true), Action::Allow);
    }

    #[test]
    fn open_link_only_takes_web_addresses() {
        assert!(external("https://example.com").is_ok());
        assert!(external(" https://example.com/path ").is_ok(), "surrounding blanks are trimmed");
        assert_eq!(external("https://example.com/a\nb").unwrap().as_str(), "https://example.com/ab", "line breaks can't reach the browser command");
        assert!(external("mailto:a@b.com").is_err(), "mailto is handled by the composer, not the browser");
        assert!(external("").is_err());
        assert!(external("example.com").is_err());
        assert!(external("https://").is_err());
        assert!(external(&format!("https://example.com/{}", "a".repeat(9000))).unwrap_err().contains("too long"));
        assert_eq!(act(&format!("https://example.com/{}", "a".repeat(9000))), Action::Deny);
    }

    #[test]
    fn disguised_schemes_and_targets_change_nothing() {
        // Browsers drop tabs and line breaks inside a scheme and ignore leading blanks; so does the URL parser here.
        for u in ["java\tscript:alert(1)", "java\nscript:alert(1)", " javascript:alert(1)", "\u{1}javascript:x", "jav&#x09;ascript:x", "data:,x", "DATA:text/html,x"] {
            assert_eq!(act(u), Action::Deny, "{u:?}");
        }
        // The same decision applies whether the click navigated the frame or asked for a new window
        // (target=_blank, _top, _parent, named): both handlers call classify().
        assert!(matches!(act("https://example.com/?a=<script>"), Action::OpenExternal(_)));
        assert_eq!(act("https://"), Action::Deny);
    }

    #[test]
    fn mailto_addresses_are_checked() {
        for bad in ["mailto:", "mailto:@x.com", "mailto:x@", "mailto:a@b@c.com", "mailto:a b@c.com", "mailto:a%0Ab@c.com", "mailto:%3Cscript%3E@x.com", "mailto:a%3Bb@c.com"] {
            assert_eq!(act(bad), Action::Deny, "{bad}");
        }
        // several recipients: the first one is used
        assert_eq!(act("mailto:a@b.com,c@d.com"), Action::Compose("a@b.com".into()));
    }
}
