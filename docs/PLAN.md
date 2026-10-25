# Graphite Mail — Linux-first offline Office 365 mail & calendar client

Scope v1: Mail + Calendar, Microsoft Graph only. SQLCipher deferred. Windows optional.

| # | Milestone | Status |
|---|-----------|--------|
| 1 | Scaffold Tauri/React, Linux deps, .deb build pipeline | done |
| 2 | OAuth2 PKCE (loopback) + tokens in GNOME Keyring | done |
| 3 | First-run wizard: choose data folder, bootstrap.json, SQLite schema + FTS5 | done |
| 4 | Graph folder + message delta sync, body/attachment hydration (CAS blobs) | done |
| 5 | 3-pane UI (virtualized list, sandboxed HTML reader, light/dark), offline search | done |
| **6** | **50% checkpoint — .deb for Faisal to test on Ubuntu** | done |
| 7 | Offline mutation queue: read/flag/move/delete, compose/send/drafts, replay-before-pull | done |
| 8 | Calendar: calendarView delta, month/week views, RSVP, TZ mapping | done |
| 9 | Crash-safe data-folder migration (7-phase) | done |
| 10 | Linux release: .deb + AppImage, icon, user guide | done |
| 11 | (optional) Windows build | |

**v1.0.0 released** — tested on Ubuntu 24.04 with a real Microsoft 365 account.

## v1.2 — multiple accounts and Gmail

- [x] Multi-account backend (per-account keyring tokens, sync, queue, All Inboxes)
- [x] Multi-account UI (grouped sidebar, From picker, add/remove account, New Folder per account)
- [x] Gmail: sign-in (desktop client, PKCE), labels → folders, history-based sync, send/reply, star/read/move/trash, Google Calendar
- [x] Gmail tested with a real account (Faisal, v1.2.1)

## v1.3 — rules
- [x] Server-side rules: Outlook inbox rules + Gmail filters (conditions: from, subject, to/cc, words; actions: move, mark read, flag/star/important, delete)
- [x] "Run on existing mail" (applied locally, sent to the server through the offline queue)
- [ ] Tested with real accounts (rules)

## v1.4 — new logo and splash
- [x] New logo (vector sources in `assets/`), app icon regenerated, README logo
- [x] Animated splash screen; the main window stays hidden until the animation (3.4 s) has finished and the UI is ready

## v1.5 — attachments
- [x] Attachment list in the reader, open / save, downloaded on demand and kept offline (Outlook + Gmail)
- [x] Safe-open: scripts and programs can only be saved
- [x] Attach files when writing (Outlook draft + upload session for big files; Gmail MIME), forwards keep the original's files

## v1.6 — signatures, drafts, bigger window buttons
- [x] Signature per account (new, reply, forward; follows the From account)
- [x] Drafts saved on this computer (autosave, save-on-cancel, Drafts list)
- [x] Larger minimise / maximise / close buttons

## v1.7 — security review fixes
- [x] Google client secret removed from the source (build-time env var or untracked `google_oauth.json`); tests guard against it coming back
- [x] Strict Content-Security-Policy (production) + dev policy; links leave the email frame as refused new-window requests
- [x] Links: only http, https and mailto are acted on (`links.rs`, unit-tested for every dangerous scheme)
- [x] Capabilities trimmed to what the UI uses (no `opener:default`, no `notification:default`, no `core:default`); test keeps them exact

## v1.8 — multi-select, themes, right-click menu
- [x] Multi-select (Ctrl/Shift-click, checkboxes, Ctrl+A) for delete, move, read/unread, flag — through the offline queue
- [x] Themes: White, Light/Medium/Dark Grey, Black, Colourful
- [x] Right-click menu: reply, forward, delete, move/copy, read/unread, flag, create rule, block sender, mark as spam

## v1.9 — Gmail with an app password (IMAP/SMTP)
- [x] "Gmail (app password)" account type: no OAuth client or client secret involved, works in every build
- [x] Setup checks IMAP and SMTP before saving; clear errors for a wrong password, a normal password instead of an app password, IMAP off, offline
- [x] App password only in the OS keyring (redacted from errors, never logged); tests guard both
- [x] IMAP (imap.gmail.com:993, TLS): folders by special-use attributes, labels as folders (one message per X-GM-MSGID), UIDVALIDITY/UID incremental sync with full resync on a UIDVALIDITY change, CONDSTORE quick check, headers first then bodies in the background or on open, attachments
- [x] Every action through the offline queue (read/unread, flag, move, copy, delete to Trash, delete for good, spam, block sender) — so multi-select works unchanged
- [x] SMTP sending (smtp.gmail.com:465) with replies threaded and forwards keeping attachments
- [x] Folder create/rename/delete over IMAP; rules for this account type run locally on new Inbox mail
- [x] End-to-end test against a pretend Gmail IMAP + SMTP server
- [ ] Tested with a real @gmail.com account and app password

## v1.9.1 — Settings → Accounts fixes (from a user screencast)
- [x] "Add Gmail (app password)" toggles the form; opening it scrolls to it and focuses the address field
- [x] Text boxes have a solid background, a visible border and a readable placeholder in all six themes (3:1 border, 4.5:1 placeholder)
- [x] Connect explains why it is inactive ("Enter both fields to continue", address format, letters typed)
- [x] Browser sign-in can be cancelled, and stops by itself after 2 minutes (also in the app's sign-in listener); the app-password button stays usable meanwhile
- [x] Builds without the Google client secret show "Google sign-in coming soon. Use an app password for now." instead of a Google button
- [x] Component tests (jsdom + Testing Library) for the above

## v1.10 — Contacts (Microsoft 365)
- [x] Contacts view: Directory + My contacts groups with counts, one search, account filter, details pane (New email / Copy email)
- [x] To/Cc suggestions from the sending Microsoft account's contacts
- [x] Graph sync: contact folders by delta (fallback: full listing), directory by paged /users (+ /me/people details), 429 Retry-After, sweep of removed entries
- [x] Separate consent (Contacts.Read People.Read User.ReadBasic.All) with Grant access; mail/calendar unaffected if it's missing or withdrawn
- [x] Local only, never logged; Remove cached contacts; deleted with the account
- [ ] Tested with a real Microsoft 365 tenant

## v1.10.1 — Contacts right-click menu
- [x] Single-contact menu (email, meeting, mail search, copy, phone, favourites, vCard, block, rule) with address/phone submenus
- [x] Multi-select (Ctrl/Shift-click, checkboxes, Ctrl+A) and its menu
- [x] Favourites group (local, per account); vCard 3.0 export; `with:` mail search by address
- [x] Submenus in the shared context menu (keyboard + mouse, kept on screen)
