//! Tests for the IMAP/SMTP account type, against pretend Gmail servers that behave like the real ones where it
//! matters (labels as folders, X-GM-MSGID, Starred, Trash/Spam/All Mail rules, Google's error texts).

use super::*;
use crate::{attachments, folders, mail, outbox, rules};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

// ---------------------------------------------------------------- unit tests

#[test]
fn folder_names_use_modified_utf7() {
    for (plain, wire) in [("Café", "Caf&AOk-"), ("日本語", "&ZeVnLIqe-"), ("A&B", "A&-B"), ("Inbox", "Inbox"), ("Ünïcödé ✓", "&ANw-n&AO8-c&APY-d&AOk- &JxM-")] {
        assert_eq!(encode_name(plain), wire);
        assert_eq!(decode_name(wire), plain);
    }
    assert_eq!(decode_name("broken&AOk"), "broken&AOk"); // unterminated: left as it is
}

#[test]
fn uid_sets_are_compact() {
    assert_eq!(uid_set([9, 1, 2, 3, 7, 10, 3]), "1:3,7,9:10");
    assert_eq!(uid_set([5]), "5");
    assert_eq!(uid_set(Vec::<u32>::new()), "");
}

#[test]
fn body_downloads_come_in_small_batches() {
    let mb = 1024 * 1024;
    let list: Vec<(u32, (), i64)> = vec![(1, (), mb), (2, (), mb), (3, (), mb), (4, (), 4 * mb), (5, (), 10)];
    let sizes: Vec<Vec<u32>> = body_batches(&list).iter().map(|b| b.iter().map(|x| x.0).collect()).collect();
    assert_eq!(sizes, vec![vec![1, 2], vec![3], vec![4], vec![5]], "about 2 MB per request, a big one on its own");
    let many: Vec<(u32, (), i64)> = (0..45).map(|i| (i, (), 1)).collect();
    assert_eq!(body_batches(&many).iter().map(|b| b.len()).collect::<Vec<_>>(), vec![20, 20, 5]);
    assert!(body_batches::<()>(&[]).is_empty());
}

#[test]
fn bookmarks_round_trip() {
    let m = Mark { validity: 7, next: 120, total: 99, modseq: Some(4242) };
    assert_eq!(Mark::parse(&m.text()), Some(m));
    let plain = Mark { modseq: None, ..m };
    assert_eq!(Mark::parse(&plain.text()), Some(plain));
    assert_eq!(Mark::parse("garbage"), None);
}

#[test]
fn app_passwords_are_cleaned_and_never_shown() {
    let s = Secret::new(" abcd efgh\tijkl mnop\n");
    assert_eq!(s.expose(), "abcdefghijklmnop");
    assert_eq!(format!("{s:?}"), "Secret([redacted])");
    let leaked = "LOGIN me abcdefghijklmnop failed; you typed abcd efgh ijkl mnop";
    let clean = s.redact(leaked);
    assert!(!clean.contains("abcdefghijklmnop") && !clean.contains("abcd efgh ijkl mnop"), "{clean}");
    assert_eq!(Secret::new("").redact("nothing to hide"), "nothing to hide");
}

#[test]
fn google_sign_in_answers_become_advice() {
    let say = |t: &str| login_problem("me@gmail.com", t, true).to_string();
    assert!(say("[ALERT] Application-specific password required: https://support.google.com/accounts/answer/185833 (Failure)").contains("2-Step Verification"));
    assert!(say("[AUTHENTICATIONFAILED] Invalid credentials (Failure)").contains("didn't accept the app password"));
    assert!(say("[ALERT] Your account is not enabled for IMAP use.").contains("IMAP is switched off"));
    // Each is a refusal (status 401), so the offline queue doesn't treat it as "offline".
    assert_eq!(login_problem("a@b.c", "x", true).downcast_ref::<Refused>().map(|r| r.status), Some(401));
}

#[test]
fn special_use_folders_are_recognised() {
    assert_eq!(special_use("INBOX", &[]), Some("inbox"));
    assert_eq!(special_use("[Gmail]/Sent Mail", &[NameAttribute::Sent]), Some("sentitems"));
    assert_eq!(special_use("[Gmail]/Spam", &[NameAttribute::Junk]), Some("junkemail"));
    assert_eq!(special_use("[Gmail]/All Mail", &[NameAttribute::All]), Some("archive"));
    assert_eq!(special_use("[Gmail]/Important", &[NameAttribute::Extension("\\Important".into())]), Some("important"));
    assert_eq!(special_use("Work", &[NameAttribute::Extension("\\HasNoChildren".into())]), None);
}

#[test]
fn the_tls_setup_builds() {
    let _ = tls_connector(); // panics if the crypto provider or root store were misconfigured
}

// ---------------------------------------------------------------- a pretend Gmail (IMAP)

const INBOX: &str = "INBOX";
const ALL: &str = "[Gmail]/All Mail";
const TRASH: &str = "[Gmail]/Trash";
const SPAM: &str = "[Gmail]/Spam";
const STARRED: &str = "[Gmail]/Starred";
const SENT: &str = "[Gmail]/Sent Mail";

struct FakeBox {
    name: String,
    attrs: String,
    validity: u32,
    next: u32,
    /// (uid, Gmail message id), by uid
    msgs: Vec<(u32, u64)>,
}

#[derive(Default)]
struct Gmail {
    /// A real Gmail (X-GM-EXT-1, "[Gmail]/…" folders, labels) or a plain IMAP server (Dovecot-like).
    gmail: bool,
    password: String,
    boxes: Vec<FakeBox>,
    raw: BTreeMap<u64, Vec<u8>>,
    flags: BTreeMap<u64, Vec<String>>,
    next_id: u64,
    modseq: u64,
    logins: usize,
    commands: Vec<String>,
}

impl Gmail {
    /// A plain IMAP server: no X-GM extension, SPECIAL-USE only on some folders, folders named the usual way.
    fn plain(password: &str) -> Gmail {
        let mut g = Gmail { gmail: false, password: password.into(), next_id: 1000, modseq: 1, ..Default::default() };
        for (name, attrs) in [
            (INBOX, "\\HasNoChildren"),
            ("Sent", "\\Sent \\HasNoChildren"),
            ("Drafts", "\\HasNoChildren"),
            ("Deleted Messages", "\\HasNoChildren"),
            ("Junk", "\\Junk \\HasNoChildren"),
            ("Archive", "\\HasNoChildren"),
            ("Projects", "\\HasNoChildren"),
        ] {
            g.create(name, attrs);
        }
        g
    }
    fn new(password: &str) -> Gmail {
        let mut g = Gmail { gmail: true, password: password.into(), next_id: 1000, modseq: 1, ..Default::default() };
        for (name, attrs) in [
            (INBOX, "\\HasNoChildren"),
            (ALL, "\\All \\HasNoChildren"),
            (SENT, "\\Sent \\HasNoChildren"),
            ("[Gmail]/Drafts", "\\Drafts \\HasNoChildren"),
            (TRASH, "\\Trash \\HasNoChildren"),
            (SPAM, "\\Junk \\HasNoChildren"),
            (STARRED, "\\Flagged \\HasNoChildren"),
            ("Work", "\\HasNoChildren"),
            ("Caf&AOk-", "\\HasNoChildren"),
        ] {
            g.create(name, attrs);
        }
        g
    }
    fn create(&mut self, name: &str, attrs: &str) {
        self.boxes.push(FakeBox { name: name.into(), attrs: attrs.into(), validity: 100 + self.boxes.len() as u32, next: 1, msgs: vec![] });
    }
    fn find(&self, name: &str) -> Option<&FakeBox> {
        self.boxes.iter().find(|b| b.name == name)
    }
    fn find_mut(&mut self, name: &str) -> Option<&mut FakeBox> {
        self.boxes.iter_mut().find(|b| b.name == name)
    }
    fn bump(&mut self) {
        self.modseq += 1;
    }
    /// The labels (mailboxes) a message is in.
    fn where_is(&self, id: u64) -> Vec<String> {
        self.boxes.iter().filter(|b| b.msgs.iter().any(|(_, g)| *g == id)).map(|b| b.name.clone()).collect()
    }
    fn add_to(&mut self, name: &str, id: u64) {
        if self.gmail && name != TRASH && name != SPAM && name != ALL && !self.where_is(id).iter().any(|n| n == ALL) {
            self.add_to(ALL, id); // every labelled message is also in All Mail
        }
        let b = self.find_mut(name).expect("mailbox");
        if !b.msgs.iter().any(|(_, g)| *g == id) {
            let uid = b.next;
            b.next += 1;
            b.msgs.push((uid, id));
        }
        self.bump();
    }
    fn remove_from(&mut self, name: &str, id: u64) {
        if let Some(b) = self.find_mut(name) {
            b.msgs.retain(|(_, g)| *g != id);
        }
        self.bump();
    }
    fn remove_everywhere(&mut self, id: u64) {
        for b in &mut self.boxes {
            b.msgs.retain(|(_, g)| *g != id);
        }
        self.bump();
    }
    /// Gmail: Trash and Spam hold a message alone.
    fn bin(&mut self, name: &str, id: u64) {
        self.remove_everywhere(id);
        self.add_to(name, id);
    }
    /// Gmail's meaning of "expunged from this mailbox".
    fn expunge_from(&mut self, name: &str, id: u64) {
        if !self.gmail {
            self.remove_from(name, id);
            if self.where_is(id).is_empty() {
                self.raw.remove(&id);
            }
            return;
        }
        match name {
            TRASH | SPAM => {
                self.remove_everywhere(id);
                self.raw.remove(&id);
            }
            ALL => self.bin(TRASH, id),
            _ => self.remove_from(name, id),
        }
    }
    fn deliver(&mut self, labels: &[&str], raw: Vec<u8>, flags: &[&str]) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.raw.insert(id, raw);
        self.flags.insert(id, flags.iter().map(|f| f.to_string()).collect());
        for l in labels {
            self.add_to(l, id);
        }
        if self.gmail && flags.contains(&"\\Flagged") {
            self.add_to(STARRED, id);
        }
        id
    }
    fn set_flag(&mut self, id: u64, flag: &str, on: bool) {
        let f = self.flags.entry(id).or_default();
        f.retain(|x| !x.eq_ignore_ascii_case(flag));
        if on {
            f.push(flag.to_string());
        }
        // Gmail: a star is the Starred label.
        if self.gmail && flag.eq_ignore_ascii_case("\\Flagged") {
            if on && !self.where_is(id).iter().any(|n| n == TRASH || n == SPAM) {
                self.add_to(STARRED, id);
            } else if !on {
                self.remove_from(STARRED, id);
            }
        }
        self.bump();
    }
    fn has_flag(&self, id: u64, flag: &str) -> bool {
        self.flags.get(&id).is_some_and(|f| f.iter().any(|x| x == flag))
    }
    /// Gives a mailbox new UIDs and a new UIDVALIDITY, as servers sometimes do.
    fn renumber(&mut self, name: &str) {
        let b = self.find_mut(name).unwrap();
        b.validity += 1000;
        let ids: Vec<u64> = b.msgs.iter().map(|(_, g)| *g).collect();
        b.msgs = ids.into_iter().enumerate().map(|(i, g)| (500 + i as u32, g)).collect();
        b.next = 500 + b.msgs.len() as u32;
        self.bump();
    }
    fn id_with_subject(&self, subject: &str) -> u64 {
        *self.raw.iter().find(|(_, r)| String::from_utf8_lossy(r).contains(&format!("Subject: {subject}\r\n"))).unwrap().0
    }
}

/// Splits an IMAP command line into arguments: atoms, "quoted strings" and (parenthesised lists).
fn toks(s: &str) -> Vec<String> {
    let cs: Vec<char> = s.chars().collect();
    let (mut out, mut i) = (vec![], 0);
    while i < cs.len() {
        match cs[i] {
            ' ' => i += 1,
            '"' => {
                let mut t = String::new();
                i += 1;
                while i < cs.len() && cs[i] != '"' {
                    if cs[i] == '\\' {
                        i += 1;
                    }
                    t.push(cs[i]);
                    i += 1;
                }
                i += 1;
                out.push(t);
            }
            '(' => {
                let (start, mut depth) = (i, 0);
                while i < cs.len() {
                    if cs[i] == '(' {
                        depth += 1;
                    }
                    if cs[i] == ')' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    i += 1;
                }
                out.push(cs[start..=i.min(cs.len() - 1)].iter().collect());
                i += 1;
            }
            _ => {
                let start = i;
                while i < cs.len() && cs[i] != ' ' {
                    i += 1;
                }
                out.push(cs[start..i].iter().collect());
            }
        }
    }
    out
}

/// The UIDs of `b` matched by a UID set like "1:4,7,9:*".
fn matching(b: &FakeBox, set: &str) -> Vec<(usize, u32, u64)> {
    let max = b.msgs.iter().map(|(u, _)| *u).max().unwrap_or(0);
    let num = |x: &str| if x == "*" { max } else { x.parse().unwrap() };
    let mut ranges = vec![];
    for part in set.split(',') {
        let (a, z) = part.split_once(':').map(|(a, z)| (num(a), num(z))).unwrap_or_else(|| (num(part), num(part)));
        ranges.push((a.min(z), a.max(z)));
    }
    b.msgs.iter().enumerate().filter(|(_, (u, _))| ranges.iter().any(|(a, z)| u >= a && u <= z)).map(|(i, (u, g))| (i + 1, *u, *g)).collect()
}

fn w(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

fn ok(out: &mut Vec<u8>, tag: &str, text: &str) {
    w(out, &format!("{tag} OK {text}\r\n"));
}

fn handle(g: &mut Gmail, selected: &mut Option<String>, line: &str) -> (Vec<u8>, bool) {
    g.commands.push(line.to_string());
    let (tag, rest) = line.split_once(' ').unwrap_or((line, ""));
    let (cmd, args) = rest.split_once(' ').unwrap_or((rest, ""));
    let mut out: Vec<u8> = vec![];
    match cmd.to_ascii_uppercase().as_str() {
        "CAPABILITY" => {
            if g.gmail {
                w(&mut out, "* CAPABILITY IMAP4rev1 UNSELECT IDLE NAMESPACE QUOTA ID XLIST CHILDREN X-GM-EXT-1 UIDPLUS COMPRESS=DEFLATE ENABLE MOVE CONDSTORE ESEARCH UTF8=ACCEPT LIST-EXTENDED LIST-STATUS LITERAL- SPECIAL-USE APPENDLIMIT=35651584\r\n");
            } else {
                w(&mut out, "* CAPABILITY IMAP4rev1 SASL-IR LOGIN-REFERRALS ID ENABLE IDLE SORT SPECIAL-USE MOVE UIDPLUS\r\n");
            }
            ok(&mut out, tag, "Success");
        }
        "LOGIN" => {
            let a = toks(args);
            if a.get(1) == Some(&g.password) {
                g.logins += 1;
                ok(&mut out, tag, &format!("{} authenticated (Success)", a[0]));
            } else if a.get(1).is_some_and(|p| p.starts_with("normal")) {
                w(&mut out, &format!("{tag} NO [ALERT] Application-specific password required: https://support.google.com/accounts/answer/185833 (Failure)\r\n"));
            } else {
                w(&mut out, &format!("{tag} NO [AUTHENTICATIONFAILED] Invalid credentials (Failure)\r\n"));
            }
        }
        "LIST" => {
            if g.gmail {
                w(&mut out, "* LIST (\\HasChildren \\Noselect) \"/\" \"[Gmail]\"\r\n");
            }
            for b in &g.boxes {
                w(&mut out, &format!("* LIST ({}) \"/\" \"{}\"\r\n", b.attrs, b.name));
            }
            ok(&mut out, tag, "Success");
        }
        "STATUS" => {
            let a = toks(args);
            match g.find(&a[0]) {
                Some(b) => {
                    let unseen = b.msgs.iter().filter(|(_, id)| !g.has_flag(*id, "\\Seen")).count();
                    w(&mut out, &format!(
                        "* STATUS \"{}\" (MESSAGES {} UNSEEN {unseen} UIDNEXT {} UIDVALIDITY {} HIGHESTMODSEQ {})\r\n",
                        b.name, b.msgs.len(), b.next, b.validity, g.modseq
                    ));
                    ok(&mut out, tag, "Success");
                }
                None => w(&mut out, &format!("{tag} NO [NONEXISTENT] Unknown Mailbox\r\n")),
            }
        }
        c @ ("SELECT" | "EXAMINE") => {
            let a = toks(args);
            match g.find(&a[0]) {
                Some(b) => {
                    w(&mut out, "* FLAGS (\\Answered \\Flagged \\Draft \\Deleted \\Seen $NotPhishing $Phishing)\r\n");
                    w(&mut out, &format!("* OK [UIDVALIDITY {}] UIDs valid.\r\n", b.validity));
                    w(&mut out, &format!("* {} EXISTS\r\n* 0 RECENT\r\n", b.msgs.len()));
                    w(&mut out, &format!("* OK [UIDNEXT {}] Predicted next UID.\r\n* OK [HIGHESTMODSEQ {}]\r\n", b.next, g.modseq));
                    *selected = Some(b.name.clone());
                    ok(&mut out, tag, if c == "SELECT" { "[READ-WRITE] Success" } else { "[READ-ONLY] Success" });
                }
                None => w(&mut out, &format!("{tag} NO [NONEXISTENT] Unknown Mailbox\r\n")),
            }
        }
        "UID" => {
            let (sub, a) = args.split_once(' ').unwrap_or((args, ""));
            let Some(sel) = selected.clone() else {
                w(&mut out, &format!("{tag} BAD No mailbox selected\r\n"));
                return (out, false);
            };
            match sub.to_ascii_uppercase().as_str() {
                "SEARCH" => {
                    let b = g.find(&sel).unwrap();
                    let hits: Vec<String> = if let Some(mid) = a.strip_prefix("HEADER Message-ID ") {
                        let mid = mid.trim_matches('"').to_ascii_lowercase();
                        b.msgs.iter().filter(|(_, id)| String::from_utf8_lossy(&g.raw[id]).to_ascii_lowercase().contains(&format!("message-id: {mid}"))).map(|(u, _)| u.to_string()).collect()
                    } else {
                        b.msgs.iter().map(|(u, _)| u.to_string()).collect() // SINCE / ALL: everything is recent here
                    };
                    w(&mut out, &format!("* SEARCH{}{}\r\n", if hits.is_empty() { "" } else { " " }, hits.join(" ")));
                    ok(&mut out, tag, "SEARCH completed (Success)");
                }
                "FETCH" => {
                    let t = toks(a);
                    let items = t[1].to_ascii_uppercase();
                    let b = g.find(&sel).unwrap();
                    for (seq, uid, id) in matching(b, &t[0]) {
                        let raw = &g.raw[&id];
                        let mut parts = vec![format!("UID {uid}")];
                        if items.contains("FLAGS") {
                            parts.push(format!("FLAGS ({})", g.flags[&id].join(" ")));
                        }
                        if items.contains("INTERNALDATE") {
                            parts.push("INTERNALDATE \"19-Oct-2026 10:00:00 +0000\"".into());
                        }
                        if items.contains("RFC822.SIZE") {
                            parts.push(format!("RFC822.SIZE {}", raw.len()));
                        }
                        if items.contains("X-GM-MSGID") {
                            parts.push(format!("X-GM-MSGID {id}"));
                        }
                        let mut lit: Option<Vec<u8>> = None;
                        if items.contains("BODY.PEEK[HEADER]") {
                            let end = raw.windows(4).position(|x| x == b"\r\n\r\n").map(|p| p + 4).unwrap_or(raw.len());
                            parts.push(format!("BODY[HEADER] {{{end}}}"));
                            lit = Some(raw[..end].to_vec());
                        } else if items.contains("BODY.PEEK[]") {
                            parts.push(format!("BODY[] {{{}}}", raw.len()));
                            lit = Some(raw.clone());
                        }
                        w(&mut out, &format!("* {seq} FETCH ({}", parts.join(" ")));
                        if let Some(l) = lit {
                            w(&mut out, "\r\n");
                            out.extend_from_slice(&l);
                            out.extend_from_slice(b")\r\n");
                        } else {
                            out.extend_from_slice(b")\r\n");
                        }
                    }
                    ok(&mut out, tag, "Success");
                }
                "STORE" => {
                    let t = toks(a);
                    let on = t[1].starts_with('+');
                    let flags: Vec<String> = t[2].trim_matches(|c| c == '(' || c == ')').split_whitespace().map(String::from).collect();
                    let hits = matching(g.find(&sel).unwrap(), &t[0]);
                    for (_, _, id) in hits {
                        for f in &flags {
                            g.set_flag(id, f, on);
                        }
                    }
                    ok(&mut out, tag, "Success");
                }
                c @ ("COPY" | "MOVE") => {
                    let t = toks(a);
                    let dest = t[1].clone();
                    if g.find(&dest).is_none() {
                        w(&mut out, &format!("{tag} NO [TRYCREATE] No folder {dest} (Failure)\r\n"));
                        return (out, false);
                    }
                    let hits = matching(g.find(&sel).unwrap(), &t[0]);
                    for (seq, _, id) in hits {
                        if !g.gmail {
                            g.add_to(&dest, id);
                            if c == "MOVE" {
                                g.remove_from(&sel, id);
                            }
                        } else if dest == TRASH || dest == SPAM {
                            g.bin(&dest, id);
                        } else {
                            g.add_to(&dest, id);
                            if c == "MOVE" {
                                if sel == TRASH || sel == SPAM { g.remove_from(&sel, id) } else { g.expunge_from(&sel, id) }
                            }
                        }
                        if c == "MOVE" {
                            w(&mut out, &format!("* {seq} EXPUNGE\r\n"));
                        }
                        let _ = seq;
                    }
                    ok(&mut out, tag, "Success");
                }
                "EXPUNGE" => {
                    let hits = matching(g.find(&sel).unwrap(), a);
                    for (_, _, id) in hits {
                        if g.has_flag(id, "\\Deleted") {
                            g.expunge_from(&sel, id);
                        }
                    }
                    ok(&mut out, tag, "Success");
                }
                _ => w(&mut out, &format!("{tag} BAD Unknown UID command\r\n")),
            }
        }
        "EXPUNGE" | "CLOSE" => {
            if let Some(sel) = selected.clone() {
                let ids: Vec<u64> = g.find(&sel).unwrap().msgs.iter().map(|(_, id)| *id).filter(|id| g.has_flag(*id, "\\Deleted")).collect();
                for id in ids {
                    g.expunge_from(&sel, id);
                }
            }
            if cmd.eq_ignore_ascii_case("CLOSE") {
                *selected = None;
            }
            ok(&mut out, tag, "Success");
        }
        "CREATE" => {
            let n = toks(args)[0].clone();
            if g.find(&n).is_some() {
                w(&mut out, &format!("{tag} NO [ALREADYEXISTS] Duplicate folder name (Failure)\r\n"));
            } else {
                g.create(&n, "\\HasNoChildren");
                ok(&mut out, tag, "Success");
            }
        }
        "RENAME" => {
            let a = toks(args);
            for b in &mut g.boxes {
                if b.name == a[0] || b.name.starts_with(&format!("{}/", a[0])) {
                    b.name = format!("{}{}", a[1], &b.name[a[0].len()..]);
                }
            }
            ok(&mut out, tag, "Success");
        }
        "DELETE" => {
            let n = toks(args)[0].clone();
            g.boxes.retain(|b| b.name != n); // Gmail keeps the messages in All Mail
            ok(&mut out, tag, "Success");
        }
        "NOOP" => ok(&mut out, tag, "Success"),
        "LOGOUT" => {
            w(&mut out, "* BYE LOGOUT Requested\r\n");
            ok(&mut out, tag, "73 good day (Success)");
            return (out, true);
        }
        _ => w(&mut out, &format!("{tag} BAD Unknown command\r\n")),
    }
    (out, false)
}

async fn fake_imap(g: Arc<Mutex<Gmail>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { break };
            let g = g.clone();
            tokio::spawn(async move {
                let (rd, mut wr) = sock.into_split();
                let mut rd = BufReader::new(rd);
                let greeting: &[u8] = if g.lock().unwrap().gmail { b"* OK Gimap ready for requests from 127.0.0.1\r\n" } else { b"* OK [CAPABILITY IMAP4rev1 SPECIAL-USE MOVE UIDPLUS] Dovecot ready.\r\n" };
                let _ = wr.write_all(greeting).await;
                let mut selected = None;
                let mut line = String::new();
                loop {
                    line.clear();
                    if rd.read_line(&mut line).await.unwrap_or(0) == 0 {
                        break;
                    }
                    let (out, close) = handle(&mut g.lock().unwrap(), &mut selected, line.trim_end());
                    if wr.write_all(&out).await.is_err() || close {
                        break;
                    }
                }
            });
        }
    });
    addr
}

// ---------------------------------------------------------------- a pretend Gmail (SMTP)

#[derive(Default)]
struct Outgoing {
    password: String,
    refuse: bool,
    /// (signed-in user, MAIL FROM, RCPT TOs, message)
    sent: Vec<(String, String, Vec<String>, String)>,
}

async fn fake_smtp(s: Arc<Mutex<Outgoing>>) -> String {
    use base64::Engine;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { break };
            let s = s.clone();
            tokio::spawn(async move {
                let (rd, mut wr) = sock.into_split();
                let mut rd = BufReader::new(rd);
                let _ = wr.write_all(b"220 smtp.gmail.test ESMTP ready\r\n").await;
                let (mut user, mut from, mut rcpts) = (String::new(), String::new(), vec![]);
                let mut line = String::new();
                loop {
                    line.clear();
                    if rd.read_line(&mut line).await.unwrap_or(0) == 0 {
                        break;
                    }
                    let l = line.trim_end().to_string();
                    let up = l.to_ascii_uppercase();
                    let reply: String = if up.starts_with("EHLO") || up.starts_with("HELO") {
                        "250-smtp.gmail.test at your service\r\n250-SIZE 35882577\r\n250-8BITMIME\r\n250-AUTH LOGIN PLAIN\r\n250 SMTPUTF8\r\n".into()
                    } else if up.starts_with("AUTH PLAIN") {
                        let b64 = match l.split_whitespace().nth(2) {
                            Some(x) => x.to_string(),
                            None => {
                                let _ = wr.write_all(b"334 \r\n").await;
                                let mut next = String::new();
                                rd.read_line(&mut next).await.unwrap();
                                next.trim().to_string()
                            }
                        };
                        let raw = base64::engine::general_purpose::STANDARD.decode(b64).unwrap_or_default();
                        let parts: Vec<String> = raw.split(|b| *b == 0).map(|p| String::from_utf8_lossy(p).to_string()).collect();
                        let (u, p) = (parts.get(1).cloned().unwrap_or_default(), parts.get(2).cloned().unwrap_or_default());
                        let g = s.lock().unwrap();
                        if g.refuse || p != g.password {
                            "535-5.7.8 Username and Password not accepted. For more information, go to\r\n535 5.7.8  https://support.google.com/mail/?p=BadCredentials\r\n".into()
                        } else {
                            user = u;
                            "235 2.7.0 Accepted\r\n".into()
                        }
                    } else if up.starts_with("MAIL FROM:") {
                        from = l[10..].trim().trim_start_matches('<').split('>').next().unwrap_or_default().to_string();
                        rcpts.clear();
                        "250 2.1.0 OK\r\n".into()
                    } else if up.starts_with("RCPT TO:") {
                        rcpts.push(l[8..].trim().trim_start_matches('<').split('>').next().unwrap_or_default().to_string());
                        "250 2.1.5 OK\r\n".into()
                    } else if up == "DATA" {
                        let _ = wr.write_all(b"354 Go ahead\r\n").await;
                        let mut data = String::new();
                        loop {
                            let mut dl = String::new();
                            if rd.read_line(&mut dl).await.unwrap_or(0) == 0 || dl == ".\r\n" {
                                break;
                            }
                            data.push_str(&dl);
                        }
                        s.lock().unwrap().sent.push((user.clone(), from.clone(), rcpts.clone(), data));
                        "250 2.0.0 OK queued\r\n".into()
                    } else if up == "QUIT" {
                        let _ = wr.write_all(b"221 2.0.0 closing connection\r\n").await;
                        break;
                    } else {
                        "250 2.0.0 OK\r\n".into() // NOOP, RSET
                    };
                    if wr.write_all(reply.as_bytes()).await.is_err() {
                        break;
                    }
                }
                let _ = rd.read_to_end(&mut vec![]).await;
            });
        }
    });
    addr
}

// ---------------------------------------------------------------- end to end

fn mail(id: &str, from: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "Message-ID: <{id}@mail.test>\r\nFrom: {from}\r\nTo: me@gmail.com\r\nSubject: {subject}\r\nDate: Mon, 19 Oct 2026 10:00:00 +0000\r\n\
         MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

fn mail_with_file(id: &str, from: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "Message-ID: <{id}@mail.test>\r\nFrom: {from}\r\nTo: me@gmail.com\r\nSubject: {subject}\r\nDate: Mon, 19 Oct 2026 11:00:00 +0000\r\n\
         MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n\
         --b1\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\nJVBERi1yZXBvcnQtYnl0ZXM=\r\n--b1--\r\n"
    )
    .into_bytes()
}

const PASSWORD: &str = "abcdefghijklmnop";

fn q<T: rusqlite::types::FromSql>(sql: &str, p: impl rusqlite::Params) -> T {
    storage::with_db(|db| Ok(db.query_row(sql, p, |r| r.get(0))?)).unwrap()
}

fn local_id(subject: &str) -> String {
    q("SELECT id FROM messages WHERE subject=?1", [subject])
}

fn folder_id(name: &str) -> String {
    q("SELECT id FROM folders WHERE display_name=?1", [name])
}

/// The folders a message shows under here.
fn shown_in(subject: &str) -> Vec<String> {
    storage::with_db(|db| {
        let mut st = db.prepare(
            "SELECT DISTINCT f.display_name FROM messages m JOIN folders f ON f.id=m.folder_id OR f.id IN (SELECT folder_id FROM message_labels WHERE message_id=m.id)
             WHERE m.subject=?1 ORDER BY f.display_name",
        )?;
        let rows = st.query_map([subject], |r| r.get(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    })
    .unwrap()
}

#[tokio::test]
async fn gmail_with_an_app_password_end_to_end() {
    let _guard = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("gm-imap-{}", uuid::Uuid::new_v4()));
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", tmp.join("cfg"));
        // This account type must work without any Google OAuth client configured.
        std::env::remove_var("GRAPHITE_GOOGLE_CLIENT_SECRET");
        std::env::remove_var("GRAPHITE_GOOGLE_CLIENT_ID");
        std::env::remove_var("GM_TEST_TOKEN"); // set by other tests; would skip the real token lookup
    }
    storage::initialize(tmp.join("vault").to_str().unwrap()).unwrap();
    let _ = auth::sign_out();

    let g = Arc::new(Mutex::new(Gmail::new(PASSWORD)));
    {
        let mut s = g.lock().unwrap();
        s.deliver(&[INBOX, "Work"], mail("m1", "Boss <boss@corp.test>", "Budget", "Please review the budget."), &[]);
        s.deliver(&[INBOX], mail_with_file("m2", "Ana <ana@x.test>", "Quarterly report", "The quarterly numbers are attached."), &[]);
        s.deliver(&[ALL], mail("m3", "Old Friend <friend@x.test>", "Archived note", "From the archive."), &["\\Seen"]);
        s.deliver(&[SENT], mail("m4", "me@gmail.com", "Sent by me", "Something I sent."), &["\\Seen"]);
    }
    let out = Arc::new(Mutex::new(Outgoing { password: PASSWORD.into(), ..Default::default() }));
    let imap_addr = fake_imap(g.clone()).await;
    let smtp_addr = fake_smtp(out.clone()).await;
    unsafe {
        std::env::set_var("GM_TEST_IMAP", &imap_addr);
        std::env::set_var("GM_TEST_SMTP", &smtp_addr);
    }

    // 1. Adding the account: clear errors, and the password never appears in them.
    let mut errors = vec![];
    for (email, pw, expect) in [
        ("me", "abcd efgh ijkl mnop", "full Gmail address"),
        ("me@gmail.com", "", "Paste the app password"),
        ("me@gmail.com", "abcd efgh", "16 letters"),
        ("me@gmail.com", "wxyz wxyz wxyz wxyz", "didn't accept the app password"),
        ("me@gmail.com", "normalpassword12", "2-Step Verification"),
    ] {
        let e = add_account(email, pw, None).await.unwrap_err().to_string();
        assert!(e.contains(expect), "{email}/{pw}: {e}");
        errors.push(e);
    }
    assert!(errors[4].contains("myaccount.google.com/apppasswords"));
    unsafe { std::env::set_var("GM_TEST_IMAP", "127.0.0.1:1") };
    let e = add_account("me@gmail.com", "abcd efgh ijkl mnop", None).await.unwrap_err().to_string();
    assert!(e.contains("Couldn't reach"), "{e}");
    errors.push(e);
    unsafe { std::env::set_var("GM_TEST_IMAP", &imap_addr) };
    out.lock().unwrap().refuse = true;
    let e = add_account("me@gmail.com", "abcd efgh ijkl mnop", None).await.unwrap_err().to_string();
    assert!(e.contains("sending"), "{e}");
    errors.push(e);
    out.lock().unwrap().refuse = false;
    for e in &errors {
        assert!(!e.contains(PASSWORD) && !e.contains("abcd efgh ijkl mnop") && !e.contains("normalpassword12"), "password in error: {e}");
    }
    assert!(auth::accounts().is_empty(), "nothing is saved until both servers accept the password");
    assert!(auth::app_password("me@gmail.com").is_err());

    let acc = add_account("  Me@Gmail.com ", "abcd efgh ijkl mnop", None).await.unwrap();
    assert_eq!((acc.email.as_str(), acc.provider.as_str(), acc.rules), ("me@gmail.com", PROVIDER, true));
    assert_eq!(auth::app_password("me@gmail.com").unwrap(), PASSWORD, "kept in the keyring, without the spaces");
    assert_eq!(auth::accounts().len(), 1);
    // An app-password account never asks for an OAuth token.
    assert!(auth::in_account(Some(acc.email.clone()), auth::access_token()).await.unwrap_err().to_string().contains("app password"));

    // 2. First sync: folders by special use, one message per Gmail message, bodies and attachments.
    let rep = sync::sync_all().await.unwrap();
    assert!(rep.messages_changed >= 4 && rep.bodies_downloaded == 4, "{} {}", rep.messages_changed, rep.bodies_downloaded);
    let fl: BTreeMap<String, Option<String>> = mail::folders().unwrap().into_iter().map(|f| {
        let v = serde_json::to_value(&f).unwrap();
        (v["name"].as_str().unwrap().to_string(), v["well_known"].as_str().map(String::from))
    }).collect();
    let wk = |n: &str| fl.get(n).cloned().unwrap_or_else(|| panic!("no folder {n}: {fl:?}"));
    assert_eq!(wk("Inbox").as_deref(), Some("inbox"));
    assert_eq!(wk("Sent Mail").as_deref(), Some("sentitems"));
    assert_eq!(wk("Drafts").as_deref(), Some("drafts"));
    assert_eq!(wk("Trash").as_deref(), Some("deleteditems"));
    assert_eq!(wk("Spam").as_deref(), Some("junkemail"));
    assert_eq!(wk("All Mail").as_deref(), Some("archive"));
    assert_eq!(wk("Starred").as_deref(), Some("flagged"));
    assert_eq!(wk("Work"), None);
    assert_eq!(wk("Café"), None, "non-ASCII label names are decoded");
    assert!(!fl.contains_key("[Gmail]"), "the \\Noselect parent is not a folder");
    assert_eq!(q::<i64>("SELECT COUNT(*) FROM messages", []), 4, "a message under several labels is stored once");
    assert_eq!(shown_in("Budget"), vec!["All Mail", "Inbox", "Work"]);
    assert_eq!(q::<String>("SELECT f.display_name FROM messages m JOIN folders f ON f.id=m.folder_id WHERE subject='Budget'", []), "Inbox");
    assert_eq!(q::<String>("SELECT f.display_name FROM messages m JOIN folders f ON f.id=m.folder_id WHERE subject='Archived note'", []), "All Mail");
    assert_eq!(mail::messages(&folder_id("Work"), 0, 50).unwrap().len(), 1);
    assert_eq!(q::<i64>("SELECT unread_count FROM folders WHERE display_name='Inbox'", []), 2);
    let m2 = local_id("Quarterly report");
    assert!(q::<String>("SELECT snippet FROM messages WHERE id=?1", [&m2]).contains("quarterly numbers"));
    assert_eq!(mail::search("quarterly", 10).unwrap().len(), 1, "bodies are searchable offline");
    let files = attachments::list(&m2).await.unwrap();
    assert_eq!((files.len(), files[0].name.as_str()), (1, "report.pdf"));
    let (_, bytes) = attachments::bytes(&files[0].id).await.unwrap();
    assert_eq!(bytes, b"%PDF-report-bytes");
    // The app password is in no file of the data folder.
    for f in ["db/mail_core.sqlite", "db/mail_core.sqlite-wal"] {
        if let Ok(b) = std::fs::read(tmp.join("vault").join(f)) {
            assert!(!b.windows(PASSWORD.len()).any(|w| w == PASSWORD.as_bytes()), "password found in {f}");
        }
    }
    // Nothing changed on the server: the next pass looks at each folder's STATUS only.
    let before = g.lock().unwrap().commands.len();
    sync::sync_all().await.unwrap();
    let cmds: Vec<String> = g.lock().unwrap().commands[before..].to_vec();
    assert!(!cmds.iter().any(|c| c.contains(" FETCH ")), "unchanged folders aren't fetched: {cmds:?}");

    // 3. Several messages at once, through the offline queue (as multi-select does).
    let m1 = local_id("Budget");
    let m3 = local_id("Archived note");
    outbox::set_read(&m1, true).unwrap();
    outbox::set_read(&m2, true).unwrap();
    outbox::set_flag(&m1, true).unwrap();
    outbox::move_to(&m2, &folder_id("Work")).unwrap();
    outbox::copy_to(&m3, &folder_id("Café")).unwrap();
    assert_eq!(outbox::pending_count().unwrap(), 5);
    assert_eq!(shown_in("Archived note"), vec!["All Mail", "Café"], "a copy shows under the label at once");
    assert_eq!(outbox::replay().await.unwrap(), 5);
    assert_eq!(outbox::pending_count().unwrap(), 0);
    {
        let s = g.lock().unwrap();
        let (g1, g2, g3) = (s.id_with_subject("Budget"), s.id_with_subject("Quarterly report"), s.id_with_subject("Archived note"));
        assert!(s.has_flag(g1, "\\Seen") && s.has_flag(g1, "\\Flagged") && s.has_flag(g2, "\\Seen"));
        assert!(s.where_is(g1).contains(&STARRED.to_string()));
        assert_eq!(s.where_is(g2), vec![ALL.to_string(), "Work".to_string()], "moved out of the Inbox into Work");
        assert!(s.where_is(g3).contains(&"Caf&AOk-".to_string()) && s.where_is(g3).contains(&ALL.to_string()), "copying from All Mail adds the label and keeps it");
    }
    sync::sync_all().await.unwrap();
    assert_eq!(shown_in("Quarterly report"), vec!["All Mail", "Work"]);
    assert!(shown_in("Budget").contains(&"Starred".to_string()));
    assert_eq!(q::<i64>("SELECT is_flagged FROM messages WHERE id=?1", [&m1]), 1);

    // 4. Delete (to Trash), delete for good, mark as spam.
    outbox::delete(&m1).unwrap();
    outbox::mark_spam(&m3).unwrap();
    outbox::replay().await.unwrap();
    {
        let s = g.lock().unwrap();
        assert_eq!(s.where_is(s.id_with_subject("Budget")), vec![TRASH.to_string()]);
        assert_eq!(s.where_is(s.id_with_subject("Archived note")), vec![SPAM.to_string()]);
    }
    sync::sync_all().await.unwrap();
    assert_eq!(shown_in("Budget"), vec!["Trash"]);
    assert_eq!(shown_in("Archived note"), vec!["Spam"]);
    outbox::delete(&m1).unwrap(); // already in Trash: gone for good
    assert_eq!(outbox::replay().await.unwrap(), 1);
    assert!(!g.lock().unwrap().raw.values().any(|r| String::from_utf8_lossy(r).contains("Subject: Budget")));
    sync::sync_all().await.unwrap();
    assert_eq!(q::<i64>("SELECT COUNT(*) FROM messages WHERE subject='Budget'", []), 0);

    // 5. Changes made elsewhere: new mail, a star, a message removed.
    {
        let mut s = g.lock().unwrap();
        s.deliver(&[INBOX], mail("m5", "Ana <ana@x.test>", "Lunch?", "Lunch on Friday?"), &[]);
        let g2 = s.id_with_subject("Quarterly report");
        s.set_flag(g2, "\\Flagged", true);
        let g4 = s.id_with_subject("Sent by me");
        s.remove_everywhere(g4);
    }
    sync::sync_all().await.unwrap();
    assert_eq!(shown_in("Lunch?"), vec!["All Mail", "Inbox"]);
    assert_eq!(q::<i64>("SELECT body_cached FROM messages WHERE subject='Lunch?'", []), 1);
    assert_eq!(q::<i64>("SELECT is_flagged FROM messages WHERE id=?1", [&m2]), 1);
    assert_eq!(q::<i64>("SELECT COUNT(*) FROM messages WHERE subject='Sent by me'", []), 0);

    // 6. The server renumbers the Inbox (new UIDVALIDITY): no duplicates, and changes still reach the message.
    g.lock().unwrap().renumber(INBOX);
    sync::sync_all().await.unwrap();
    assert_eq!(q::<i64>("SELECT COUNT(*) FROM messages WHERE subject='Lunch?'", []), 1);
    let m5 = local_id("Lunch?");
    outbox::set_read(&m5, true).unwrap();
    outbox::replay().await.unwrap();
    {
        let s = g.lock().unwrap();
        assert!(s.has_flag(s.id_with_subject("Lunch?"), "\\Seen"));
    }

    // 7. Block sender: kept here as a rule, applied to new mail at sync.
    assert_eq!(rules::block_sender(&m5).await.unwrap(), "ana@x.test");
    assert_eq!(q::<Option<String>>("SELECT remote_id FROM rules", []), None, "no server rule for app-password accounts");
    g.lock().unwrap().deliver(&[INBOX], mail("m6", "Ana <ana@x.test>", "Buy now", "Special offer."), &[]);
    sync::sync_all().await.unwrap();
    assert_eq!(shown_in("Buy now"), vec!["Spam"], "the rule moved it here at once");
    outbox::replay().await.unwrap();
    {
        let s = g.lock().unwrap();
        assert_eq!(s.where_is(s.id_with_subject("Buy now")), vec![SPAM.to_string()]);
    }

    // 8. Sending: a reply joins the conversation; a forward carries the original's attachment.
    outbox::send(outbox::Draft { mode: "reply".into(), reply_to: Some(m5.clone()), to: vec!["ana@x.test".into()], cc: vec!["cc@x.test".into()], bcc: vec!["hidden@x.test".into()], subject: "Re: Lunch?".into(), html: "<p>Yes!</p>".into(), from: None, attachments: vec![] }).unwrap();
    outbox::send(outbox::Draft { mode: "forward".into(), reply_to: Some(m2.clone()), to: vec!["bob@x.test".into()], cc: vec![], bcc: vec![], subject: "Fw: Quarterly report".into(), html: "<p>FYI</p>".into(), from: None, attachments: vec![] }).unwrap();
    assert_eq!(outbox::replay().await.unwrap(), 2);
    {
        let o = out.lock().unwrap();
        assert_eq!(o.sent.len(), 2);
        let (user, from, rcpts, data) = &o.sent[0];
        assert_eq!((user.as_str(), from.as_str()), ("me@gmail.com", "me@gmail.com"));
        // The blind copy is an SMTP envelope recipient (RCPT TO) and appears nowhere in the message itself.
        assert_eq!(rcpts, &vec!["ana@x.test".to_string(), "cc@x.test".to_string(), "hidden@x.test".to_string()]);
        assert!(!data.contains("hidden@x.test") && !data.to_ascii_lowercase().contains("bcc"), "no Bcc header over SMTP: {data}");
        assert!(data.contains("To: <ana@x.test>") && data.contains("Cc: <cc@x.test>"), "{data}");
        assert!(data.contains("In-Reply-To: <m5@mail.test>") && data.contains("Subject: Re: Lunch?"), "{data}");
        assert!(data.contains("From: <me@gmail.com>") && !data.contains("\"\""), "no empty display name: {data}");
        assert!(o.sent[1].3.contains("report.pdf"), "forward keeps the attachment");
    }
    // A recipient the server can't take is refused for good, never sent twice.
    outbox::send(outbox::Draft { mode: "new".into(), reply_to: None, to: vec!["not an address".into()], cc: vec![], bcc: vec![], subject: "x".into(), html: "x".into(), from: Some("me@gmail.com".into()), attachments: vec![] }).unwrap();
    outbox::replay().await.unwrap();
    assert!(outbox::overview().unwrap()[0].failed);
    outbox::discard_failed().unwrap();

    // 9. A body that wasn't downloaded yet comes when the message is opened.
    storage::with_db(|db| Ok(db.execute("UPDATE messages SET body_cached=0, body_blob_hash=NULL WHERE id=?1", [&m5])?)).unwrap();
    ensure_body(&m5).await.unwrap();
    assert!(mail::message(&m5).unwrap().body_html.unwrap().contains("Lunch on Friday"));

    // 10. Folders: create, rename, delete on the server.
    let pid = folders::create("Projects", None, Some("me@gmail.com")).await.unwrap();
    assert!(g.lock().unwrap().find("Projects").is_some());
    folders::rename(&pid, "Ideas 2026").await.unwrap();
    assert!(g.lock().unwrap().find("Ideas 2026").is_some() && g.lock().unwrap().find("Projects").is_none());
    assert_eq!(q::<String>("SELECT remote_id FROM folders WHERE id=?1", [&pid]), "Ideas 2026");
    assert!(folders::rename(&folder_id("Inbox"), "Nope").await.is_err(), "built-in folders keep their names");
    folders::delete(&pid).await.unwrap();
    assert!(g.lock().unwrap().find("Ideas 2026").is_none());

    // 11. Offline: nothing is lost; it goes out when the server is back.
    forget("me@gmail.com");
    unsafe { std::env::set_var("GM_TEST_IMAP", "127.0.0.1:1") };
    outbox::set_read(&m5, false).unwrap();
    assert!(outbox::replay().await.is_err());
    assert_eq!(outbox::pending_count().unwrap(), 1);
    unsafe { std::env::set_var("GM_TEST_IMAP", &imap_addr) };
    assert_eq!(outbox::replay().await.unwrap(), 1);

    // 12. The app password was revoked: changes wait (with advice) instead of being dropped.
    forget("me@gmail.com");
    g.lock().unwrap().password = "revoked-password".into();
    outbox::set_read(&m5, true).unwrap();
    outbox::replay().await.unwrap();
    let item = &outbox::overview().unwrap()[0];
    assert!(!item.failed && item.error.as_deref().unwrap_or_default().contains("app password"), "{:?}", item.error);
    outbox::discard_failed().unwrap();
    storage::with_db(|db| Ok(db.execute("DELETE FROM offline_mutation_queue", [])?)).unwrap();

    // 13. Removing the account forgets the password and the downloaded mail.
    crate::remove_account("me@gmail.com".into()).unwrap();
    assert!(auth::app_password("me@gmail.com").is_err());
    assert!(auth::accounts().is_empty());
    assert_eq!(q::<i64>("SELECT COUNT(*) FROM messages", []), 0);
    assert_eq!(q::<i64>("SELECT COUNT(*) FROM imap_uids", []), 0);
    unsafe {
        std::env::remove_var("GM_TEST_IMAP");
        std::env::remove_var("GM_TEST_SMTP");
    }
}

#[test]
fn common_folder_names_give_roles_on_plain_servers() {
    for (name, role) in [("Sent", "sentitems"), ("Sent Items", "sentitems"), ("Sent Messages", "sentitems"), ("Drafts", "drafts"), ("Trash", "deleteditems"),
        ("Deleted Messages", "deleteditems"), ("Deleted Items", "deleteditems"), ("Bin", "deleteditems"), ("Junk", "junkemail"), ("Junk E-mail", "junkemail"),
        ("Spam", "junkemail"), ("Bulk Mail", "junkemail"), ("Archive", "archive"), ("ARCHIVES", "archive")] {
        assert_eq!(role_by_name(name), Some(role), "{name}");
    }
    for name in ["Projects", "INBOX", "Sent to Ann", "Work", "Caf&AOk-"] {
        assert_eq!(role_by_name(name), None, "{name}");
    }
}

/// "Other email (IMAP)": a Dovecot-like server with no Gmail extension and the usual folder names.
#[tokio::test]
async fn other_email_over_plain_imap_end_to_end() {
    let _guard = storage::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = std::env::temp_dir().join(format!("gm-other-{}", uuid::Uuid::new_v4()));
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", tmp.join("cfg"));
        std::env::remove_var("GM_TEST_TOKEN");
        std::env::remove_var("GM_TEST_IMAP");
        std::env::remove_var("GM_TEST_SMTP");
    }
    storage::initialize(tmp.join("vault").to_str().unwrap()).unwrap();
    let _ = auth::sign_out();

    let g = Arc::new(Mutex::new(Gmail::plain("s3cret-app-pw")));
    {
        let mut s = g.lock().unwrap();
        s.deliver(&[INBOX], mail("o1", "Boss <boss@corp.test>", "Budget", "Please review the budget."), &[]);
        s.deliver(&[INBOX], mail("o2", "Ana <ana@x.test>", "Lunch?", "Lunch on Friday?"), &[]);
        s.deliver(&["Archive"], mail("o3", "Old <old@x.test>", "Archived note", "From the archive."), &["\\Seen"]);
        s.deliver(&["Sent"], mail("o4", "me@example.net", "Sent by me", "Something I sent."), &["\\Seen"]);
    }
    let out = Arc::new(Mutex::new(Outgoing { password: "s3cret-app-pw".into(), ..Default::default() }));
    let imap_addr = fake_imap(g.clone()).await;
    let smtp_addr = fake_smtp(out.clone()).await;
    let (ih, ip) = imap_addr.rsplit_once(':').unwrap();
    let (sh, sp) = smtp_addr.rsplit_once(':').unwrap();
    let settings = ImapSettings {
        preset: "custom".into(), imap_host: ih.into(), imap_port: ip.parse().unwrap(), imap_tls: "none".into(),
        smtp_host: sh.into(), smtp_port: sp.parse().unwrap(), smtp_tls: "none".into(), username: "me@example.net".into(),
    };

    // Adding: checks before connecting, then both servers; no Gmail wording; passwords of any shape.
    let e = add_account("me", "x", Some(settings.clone())).await.unwrap_err().to_string();
    assert!(e.contains("full email address") && !e.contains("Gmail"), "{e}");
    let e = add_account("me@example.net", "", Some(settings.clone())).await.unwrap_err().to_string();
    assert!(e.contains("app password") && !e.contains("Google"), "{e}");
    let bad = ImapSettings { imap_host: "".into(), ..settings.clone() };
    assert!(add_account("me@example.net", "s3cret-app-pw", Some(bad)).await.unwrap_err().to_string().contains("IMAP server"));
    let e = add_account("me@example.net", "wrong", Some(settings.clone())).await.unwrap_err().to_string();
    assert!(e.contains("didn't accept the sign-in") && !e.contains("Google") && !e.contains("wrong"), "{e}");
    let acc = add_account("me@example.net", "s3cret-app-pw", Some(settings.clone())).await.unwrap();
    assert_eq!((acc.provider.as_str(), acc.imap.as_ref().map(|s| s.preset.as_str())), ("imap", Some("custom")));
    assert_eq!(auth::accounts()[0].imap, Some(settings.clone()), "the servers are remembered with the account");

    // Sync: folder roles from SPECIAL-USE where given, from the usual names otherwise; nothing Gmail-only on the wire.
    let account_id = crate::sync::ensure_account_pub(&acc).unwrap();
    let rep = auth::in_account(Some(acc.email.clone()), sync_account(&account_id)).await.unwrap();
    assert!(rep.messages_changed >= 4, "{}", rep.messages_changed);
    let role = |name: &str| q::<Option<String>>("SELECT well_known_name FROM folders WHERE remote_id=?1", [name]);
    assert_eq!(role("INBOX").as_deref(), Some("inbox"));
    assert_eq!(role("Sent").as_deref(), Some("sentitems"), "from \\Sent");
    assert_eq!(role("Junk").as_deref(), Some("junkemail"), "from \\Junk");
    assert_eq!(role("Drafts").as_deref(), Some("drafts"), "by name");
    assert_eq!(role("Deleted Messages").as_deref(), Some("deleteditems"), "by name");
    assert_eq!(role("Archive").as_deref(), Some("archive"), "by name");
    assert_eq!(role("Projects"), None);
    {
        let s = g.lock().unwrap();
        assert!(!s.commands.iter().any(|c| c.contains("X-GM")), "no Gmail extension used: {:?}", s.commands.iter().filter(|c| c.contains("FETCH")).next());
    }
    assert_eq!(shown_in("Budget"), vec!["Inbox".to_string()]);
    assert_eq!(shown_in("Archived note"), vec!["Archive".to_string()]);

    // Changes: a move out of Archive really moves (no Gmail "view" rule); delete from Trash expunges.
    let m1 = local_id("Archived note");
    outbox::move_to(&m1, &folder_id("Projects")).unwrap();
    outbox::replay().await.unwrap();
    {
        let s = g.lock().unwrap();
        assert_eq!(s.where_is(s.id_with_subject("Archived note")), vec!["Projects".to_string()]);
    }
    let m2 = local_id("Lunch?");
    outbox::set_read(&m2, true).unwrap();
    outbox::move_to(&m2, &folder_id("Deleted Messages")).unwrap();
    outbox::replay().await.unwrap();
    auth::in_account(Some(acc.email.clone()), sync_account(&account_id)).await.unwrap(); // learns the message's UID in the bin
    assert_eq!(shown_in("Lunch?"), vec!["Deleted Messages".to_string()]);
    // Plain IMAP has no server-wide message id (Gmail's X-GM-MSGID): a message is known by folder + UID, so after a
    // move and a sync it is a fresh local row. Look it up again before deleting it for good.
    let m2 = local_id("Lunch?");
    outbox::delete(&m2).unwrap(); // in the bin: for good
    outbox::replay().await.unwrap();
    {
        let s = g.lock().unwrap();
        assert!(s.raw.values().all(|r| !String::from_utf8_lossy(r).contains("Subject: Lunch?")), "gone from the server");
    }

    // Sending through the account's own SMTP server with its user name.
    outbox::send(outbox::Draft { mode: "new".into(), reply_to: None, to: vec!["ana@x.test".into()], cc: vec![], bcc: vec!["hidden@x.test".into()], subject: "Hi".into(), html: "<p>hi</p>".into(), from: Some("me@example.net".into()), attachments: vec![] }).unwrap();
    assert_eq!(outbox::replay().await.unwrap(), 1);
    {
        let o = out.lock().unwrap();
        let (user, from, rcpts, data) = &o.sent[0];
        assert_eq!((user.as_str(), from.as_str()), ("me@example.net", "me@example.net"));
        assert_eq!(rcpts, &vec!["ana@x.test".to_string(), "hidden@x.test".to_string()]);
        assert!(!data.contains("hidden@x.test"));
    }
    // A Gmail account added at the same time still uses Gmail's servers (the test override) and its own rules.
    assert!(settings_of("me@example.net").is_some() && settings_of("nobody@gmail.com").is_none());
    let _ = auth::sign_out();
}
