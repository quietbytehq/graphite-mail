# Night log — overnight/v1.11

## Summary (read this first)

**All five tasks done**, each committed and pushed to `origin/overnight/v1.11` as it finished. Nothing merged to main,
no tags, no releases, no `.deb` built, the Google client secret never read, printed or used (every cargo/npm command
ran with `env -u GRAPHITE_GOOGLE_CLIENT_SECRET`).

| Task | Version | Commit | State |
|---|---|---|---|
| 1. Context-menu focus + first-run Cancel sign-in / timeout | 1.10.2 | "Safer context-menu focus and cancellable first-run sign-in" | ✅ |
| 2. Bcc for Graph, Gmail API, SMTP; drafts; autocomplete | 1.10.3 | "Bcc for Microsoft 365, Gmail and app-password accounts" | ✅ |
| 3. Windows port, NSIS, release workflow, platform tests | 1.11.0 | "Windows port: platform code, NSIS bundle, release workflow" | ✅ code + CI; real-machine checks below |
| 4. Gmail (app password) contacts: people you've emailed, imports | 1.11.0 | "Local contacts for Gmail app-password accounts" | ✅ |
| 5. README, screenshots, changelog, user guide | — | "Docs: README for Linux and Windows…" | ✅ |

**Final checks (last run, after Task 5):** `tsc --noEmit` clean · frontend **88 tests** in 13 files (was 64) ·
Rust **83 tests** (was 69) · `npm run build` OK · `cargo build` OK · `cargo check --target x86_64-pc-windows-gnu --tests` OK.

**Files changed vs main:** 65 files changed, 2499 insertions(+), 239 deletions(-).

**What could not be verified here (no Microsoft, Google or Windows reachable):**
1. Windows: everything in the Task 3 list below (installer, Credential Manager, file locking during a data-folder move,
   frameless-window resizing, toasts, MSVC build). The CI's Windows job will show the first of these when you push a tag.
2. Bcc delivery end to end: that Gmail strips the `Bcc` header for the Gmail-API path, that Outlook reply/forward with
   `bccRecipients` delivers to the blind copy. (Tests prove what each provider is *sent*.)
3. Cancel sign-in against a real browser tab (the Rust side was already tested in v1.9.1).
4. `is_automated()` against a real Gmail mailbox, and real Google Contacts / phone vCard exports.
5. The workflow itself (it is syntactically valid YAML; it has not run yet). Pushing `.github/workflows/` was accepted.

**Suggested review order:**
1. `NIGHT_LOG.md` (this file) and `CHANGELOG.md` (1.10.2, 1.10.3, 1.11.0).
2. Task 1: `src/menu.ts` (`initialFocus`), `src/ContextMenu.tsx`, `src/signIn.ts`, `src/App.tsx`; tests in
   `ContextMenu.test.tsx`, `Mailbox.test.tsx`, `App.test.tsx`.
3. Task 2: `src-tauri/src/outbox.rs` (Graph `bccRecipients`), `google.rs` (`build_raw` Bcc header), `imap.rs` (envelope
   only), `drafts.rs` + `storage.rs` migration; the three leak tests; `src/Compose.tsx`.
4. Task 3: `src-tauri/Cargo.toml` (deps), `migrate.rs` (`free_space`), `storage.rs` (`config_dir`, `default_data_dir`),
   `attachments.rs` (`safe_name`), `tauri.windows.conf.json`, `.github/workflows/release.yml`, `platform_tests.rs`.
5. Task 4: `src-tauri/src/local_contacts.rs` (+ tests), the `contacts.rs` diff (group/provider filter, refresh, suggest),
   `sync.rs` hook, `src/ContactsView.tsx`, `Settings.tsx`.
6. Task 5: `README.md`, `docs/screenshots/`, `docs/USER-GUIDE.md`.

**Decisions I made without asking (details in each task):** Bcc-only messages are allowed; the Gmail-API path sends a
`Bcc` header (the only way that API takes envelope recipients); data on Windows goes to `%LOCALAPPDATA%` (not roaming);
`XDG_CONFIG_HOME` is honoured on every platform; `ring` was not swapped in for aws-lc-rs (the workflow installs NASM
instead); imports go to the account chosen in the Contacts filter, else the first app-password account; Windows bundle
settings live in Tauri's `tauri.windows.conf.json` overlay so Linux builds are untouched; tests that asserted old
limitations (no contacts / suggestions for IMAP accounts, Remove cached contacts only for Microsoft) were updated.

**Follow-up (next morning): flaky contacts tests on the Windows runner.** The matcher `/3 messages · last/` in
`Mailbox.test.tsx` (and the group count / row assertions in `ContactsView.test.tsx`) failed on the slower Windows
runner. Not a date or locale problem: reproduced on Linux by delaying the fake backend (contacts_overview 350 ms,
everything else 120 ms). Real cause: the Contacts view renders the group headings as soon as the accounts are known
(count 0, no rows) and fills the rows only when `contacts_overview` answers, so `findByRole("group")` resolved before
the data and the following `getByText` on a row ran against an empty group. Fix: the tests now wait for a row
(`findByText`) before asserting on rows or counts; no product code changed (showing the headings while loading is
intended, the title says "Loading…"). Verified with the slowed backend (24/24) and the normal one (88/88, three runs).

**Open problems / things to watch:** Credential Manager's 2560-byte limit vs long Microsoft refresh tokens; the Windows
`cargo test` run (file locking in the migration tests); the compose screenshot shows Bcc typed but without the
suggestion list open (cosmetic); `docs/screenshots` adds 3.1 MB to the repo.

---

## Pre-check
- `git fetch origin && git checkout main && git pull` → main at 6ae8af6 (v1.10.1 tag present).
- package.json, src-tauri/Cargo.toml, src-tauri/tauri.conf.json all said **1.10.1** → continued.
- Branch `overnight/v1.11` created from main.
- Environment: `GRAPHITE_GOOGLE_CLIENT_SECRET` is set in this sandbox. It is never read, printed or committed; every
  cargo / npm build or test runs with `env -u GRAPHITE_GOOGLE_CLIENT_SECRET`. No `.deb` is produced tonight.
- The sandbox had no GTK/WebKit development libraries; installed `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev
  librsvg2-dev libayatana-appindicator3-dev patchelf` with apt (sandbox only, nothing in the repo).
- Baseline before any change: Rust 69 tests pass, frontend 64 tests pass (12 files), `tsc --noEmit` clean.

## Task 1 — v1.10.2 fixes  ✅
### 1a Mail list context menu: no destructive item gets initial focus
- Root cause: `ContextMenu` started on the first *enabled* item. With several messages selected (or before the open
  message's details arrive) Reply / Reply All / Forward are disabled, so the first enabled item was **Delete**, and a
  first Enter deleted the selection.
- Fix: `MenuItem.destructive` flag (`src/ContextMenu.tsx`); `initialFocus()` in `src/menu.ts` picks the first enabled,
  non-destructive item, or `-1` when there is none. With `-1` the menu panel itself takes focus, Enter/Space do nothing,
  ArrowDown goes to the first enabled item and ArrowUp to the last. Mail list marks Delete, Block sender, Mark as spam;
  the Contacts menu marks Block sender (`src/contactMenu.ts`).
- Tests: `menu.test.ts` (initialFocus), `ContextMenu.test.tsx` (3 cases: skips Delete, Reply first when enabled, all-
  destructive menu focuses the panel and Enter is a no-op), `Mailbox.test.tsx` (right-click a real row while Reply is
  unavailable: focus is "Move to…", Enter opens the folder picker and never calls `delete_message`; and Delete still
  works when reached on purpose). The Mailbox tests mock `@tanstack/react-virtual` because jsdom has no layout.
### 1b First-run screen: Cancel sign-in, 2-minute timeout, cleanup, no late error
- New `src/signIn.ts` → `useBrowserSignIn()` hook, extracted from the v1.9.1 Settings code (attempt counter, timer,
  `cancel_sign_in` invoke, unmount cleanup). Settings → Accounts now uses the hook (behaviour unchanged, its 6 tests
  still pass). The first-run screen (`src/App.tsx`) uses it too: "Cancel sign-in" button, timeout note, the buttons
  come back, a late error or late success from a cancelled attempt is ignored.
- "Listener cleanup" read as: the timer is cleared and the attempt retired on unmount, and the Rust redirect listener
  is told to stop through `cancel_sign_in` (already existing in `auth.rs`, tested there).
- Tests: `App.test.tsx` +5 (cancel + late error ignored, stale success doesn't open the mailbox while a fresh one does,
  timeout at exactly 120 s, unmount clears the timer, app-password path clears the note).
- Version bumped to **1.10.2** (package.json, package-lock.json, Cargo.toml, Cargo.lock, tauri.conf.json). CHANGELOG entry added.
- Test counts after Task 1: Rust 69, frontend 76 (12 files). tsc clean.
- Needs a real-world check: pressing Cancel while a real browser tab is open (the tab then shows "Sign-in was cancelled
  or failed"); menu-key behaviour inside the real WebView.

## Task 2 — Bcc (v1.10.3)  ✅
- Frontend (`src/Compose.tsx`): `bcc` state, "Bcc" link on the Cc row (`.fieldlink`, same borderless `.field` row style),
  the Bcc row appears below Cc and uses `RecipientInput` (same contact suggestions as To/Cc). Send validates all three
  lists and allows Bcc-only messages. Drafts save/restore `bcc`; `Mailbox.tsx` passes `bcc` when reopening a draft.
  Reply / reply-all / forward never pre-fill Bcc.
- Rust: `outbox::Draft.bcc` (serde default, so old queued items still parse); the queued payload carries `"bcc"`.
  - **Microsoft Graph**: `bccRecipients` on `/me/sendMail`, on `createReply/createReplyAll/createForward` + PATCH
    (attachments path), and on `/reply`, `/replyAll`, `/forward`. A forward's top-level `toRecipients` stays To only.
  - **Gmail API (Google sign-in)**: `build_raw()` takes `bcc`; it writes a `Bcc:` header because the Gmail API derives
    envelope recipients from the headers and strips Bcc on delivery (documented Gmail behaviour; cannot be observed in
    the sandbox, see "needs a real-world check").
  - **IMAP/SMTP (app password)**: `build_raw()` is called with no Bcc at all; the blind copies are added to the lettre
    `Envelope` (RCPT TO). The message data contains neither a Bcc header nor the address.
  - Drafts: `bcc_text` column, schema + `migrate()` + migration test.
- Tests (Rust +3 → 72): `outbox::blind_copies_never_leak_into_graph_to_or_cc` (new/reply/replyAll/forward against the
  fake Graph: address appears exactly once, in bccRecipients), `google::blind_copies_are_a_bcc_header_for_the_gmail_api_
  and_nothing_else`, the Gmail end-to-end test asserts the raw sent to the fake Gmail API, the IMAP end-to-end test
  asserts RCPT TO has the blind copy and the DATA has no Bcc header, the attachments test asserts the draft-with-files
  Graph body, storage migration test. Frontend +5 → 81 (Bcc link, suggestions, send payload split, Bcc-only send and
  validation, draft round trip, replies start empty).
- Decision: Bcc-only messages (no To) are allowed, as in Outlook/Gmail. If this isn't wanted, re-add the To check in
  `Compose.send()`.
- Fixed on the way: `Compose` wrote the signature into the editor after unmount (seen as an unhandled error in tests).
- Needs a real-world check: Gmail API strips the Bcc header for the other recipients (send a Bcc'd message from a
  Google-signed-in account and inspect what the To recipient receives); Outlook reply/forward with Bcc delivers to the
  blind copy; Gmail SMTP sent copy (Gmail's Sent Mail) doesn't show the Bcc (expected, same as other clients).
- Counts after Task 2: Rust 72, frontend 81. tsc clean. Version 1.10.3.

## Task 3 — Windows port (v1.11.0)  ✅ (code + CI; real-machine checks listed below)
### Audit of Linux-only assumptions (Rust + frontend) and what changed
| Area | Finding | Change |
|---|---|---|
| Free-space check (`migrate.rs`) | `libc::statvfs` + `std::os::unix` | `#[cfg(unix)]` statvfs, `#[cfg(windows)]` `GetDiskFreeSpaceExW` (windows-sys 0.61, already in the lock file). `libc` is now a unix-only dependency. |
| Keyring (`Cargo.toml`) | only `sync-secret-service` | added `windows-native` → Windows Credential Manager. Each keyring feature compiles only on its own platform. |
| Data folder default (`storage.rs`) | `dirs::data_dir()` = roaming AppData on Windows | `dirs::data_local_dir()` (`%LOCALAPPDATA%\graphite-mail`; unchanged on Linux). |
| Config pointer (`storage.rs`) | `XDG_CONFIG_HOME` only honoured by `dirs` on Linux | honoured explicitly on every platform, so the tests stay in a scratch folder on the Windows runner too. |
| Attachment names (`attachments.rs`) | only `/ \ NUL` and control chars replaced | also `< > : " \| ? *`, trailing dots/spaces, device names (CON, NUL, COM1–9, LPT1–9). |
| Folder picker (`App.tsx`, `Settings.tsx`) | `${picked}/graphite-mail` | `dataFolderFrom()` in `src/paths.ts` uses the separator the picked path already has. |
| Opening links / files | already `tauri-plugin-opener` (no `xdg-open`) | none needed. |
| Notifications | already `tauri-plugin-notification` (no D-Bus / `notify-send` in app code) | none needed. |
| System theme | `matchMedia(prefers-color-scheme)` in the WebView (WebView2 supports it) | none needed. |
| File dialogs | `tauri-plugin-dialog` | none needed. |
| Shell commands | none in the code base | — |
| Window (`decorations: false`, custom buttons, drag region, splash `skip_taskbar`) | all Tauri APIs | none; see checks below. |
| Sign-in redirect | loopback `127.0.0.1:<random>` listener, browser opened through the opener plugin | none; Windows loopback needs no firewall rule. |
| `main.rs` | already has `windows_subsystem = "windows"` for release | — |
- NSIS: `src-tauri/tauri.windows.conf.json` (Tauri's per-platform overlay) sets `targets: ["nsis"]`, `installMode:
  currentUser`, `installerIcon: icons/icon.ico`, `webviewInstallMode: downloadBootstrapper` (silent). The base config
  keeps deb + appimage for Linux, so a local `tauri build` on either platform does the right thing.
- CI: `.github/workflows/release.yml` — on `v*.*.*` tags: tests on ubuntu-22.04 and windows-latest (type check, vitest,
  cargo test), then `.deb` (ubuntu-22.04) and NSIS `.exe` (windows-latest, NASM installed for aws-lc-sys) on separate
  runners, versions checked against the tag, then a **draft** release with both files attached (`gh release`, built-in
  token). Only first-party actions (checkout, setup-node, cache, upload/download-artifact). The Google client secret
  is not referenced anywhere; a step fails the job if `src-tauri/google_oauth.json` is present.
- Verification done here: `cargo check --target x86_64-pc-windows-gnu --tests` **passes** (mingw toolchain installed in
  the sandbox), so every `cfg(windows)` branch and the Windows-only tests type-check. Linux: Rust 75 tests, frontend
  83, tsc clean.
- Tests added: `src-tauri/src/platform_tests.rs` (free space on temp + not-yet-existing folder; default data folder is
  absolute, per-user, local on Windows; Windows-safe attachment names; `#[cfg(windows)]`: drive-letter paths through
  `normalize()`, keyring round trip in the Credential Manager — these two run on the Windows CI runner), `src/paths.test.ts`.
### Can only be verified on a real Windows machine
1. The NSIS installer: per-user install path (`%LOCALAPPDATA%\Programs\Graphite Mail`), Start-menu shortcut, icon,
   uninstaller, WebView2 bootstrapper when the runtime is missing.
2. First run: config pointer in `%APPDATA%\graphite-mail`, data in `%LOCALAPPDATA%\graphite-mail`, folder picker with a
   backslash path, "Move data folder" (free-space check, rename/remove while SQLite has files open — Windows locks
   open files; `migrate.rs` closes the DB before switching, but this is the biggest risk).
3. Browser sign-in: default browser opens, redirect to `http://localhost:<port>` comes back, the window regains focus.
4. Keyring: tokens/app passwords in Credential Manager (test runs in CI). Credential Manager caps a secret at 2560
   bytes; Microsoft refresh tokens are usually 1–2 KB. If one is ever longer, sign-in would fail with a keyring
   error — worth watching.
5. Frameless window: resize from the edges, snap/maximise, the custom min/max/close buttons, double-click to maximise,
   the splash window (no taskbar entry, always on top).
6. Notifications: Windows toasts need the app registered with an AppUserModelID — the NSIS install provides the shortcut;
   running the bare `.exe` from `target/` may show nothing.
7. Attachments: open (temp folder under `%TEMP%`) and Save As; names with characters Windows refuses.
8. The `cargo test` run on the Windows runner itself (file-locking differences in the migration tests; see 2).
9. MSVC build: the sandbox cross-check used the GNU target; the CI uses MSVC (what Tauri supports). aws-lc-sys needs
   NASM + CMake there (the workflow installs NASM; CMake is on the runner image).
10. ~~Whether pushing `.github/workflows/` is accepted~~ — it was: the push with `.github/workflows/release.yml` succeeded.

## Task 4 — Gmail contacts for IMAP accounts (in v1.11.0)  ✅
- New `src-tauri/src/local_contacts.rs` (+ `local_contacts_tests.rs`). Both kinds live in the existing `contacts`
  table under the IMAP account (sources `emailed` and `imported`), so favourites (keyed by address for emailed
  people, so they survive a rebuild), the right-click menu, vCard export, `suggest_recipients` (To/Cc/Bcc), "Remove
  cached contacts" (`DELETE … WHERE account_id`) and account removal (`ON DELETE CASCADE`) needed no new plumbing.
- **People you've emailed**: `rebuild_people(account_id)` runs after each IMAP sync (`sync.rs`) and on Refresh
  contacts. Incremental: a bookmark `contacts:people:scan = "<max rowid>:<rows up to it>"` in `account_state`; only
  newer messages are read, counts are added, the name comes from the newest message that had one. If older rows were
  deleted (count mismatch) it does a full rebuild so counts never drift. Skips the account's own address, Spam/Bin/
  Drafts folders and `is_automated()` senders (no-reply variants, mailer-daemon, postmaster, bounce(s), notification(s),
  newsletter, digest, list `-request/-owner`, VERP-like long hex local parts, and names like "Mail Delivery
  Subsystem"). Columns `message_count`, `last_contacted` added to `contacts` (schema + migration).
- **Import**: `decode_text` (UTF-8 BOM, UTF-16 LE/BE BOM, UTF-8, else Windows-1252), `parse_vcards` (2.1/3.0/4.0,
  folding, QP soft breaks, `item1.` groups, bare 2.1 type words, `\,` escapes, tel: URIs), `parse_csv` (RFC 4180) +
  `parse_contacts_csv` (classic and 2023+ Google layouts, Outlook columns, ` ::: ` multi-values), `dedupe` (by address,
  nameless entries by name), `store_imported` (merges into an existing imported contact sharing an address). Files
  over 50 MB refused. Command `import_contacts { account, path }`.
- UI (`ContactsView.tsx`): groups "People you've emailed" (newest first, "addr · 12 messages · last 3 Oct 2026") and
  "Imported"; groups shown depend on the kinds of accounts present / the account filter; Google note (`role=note`)
  with the **Import contacts…** button, plus a toolbar import icon; detail pane shows Messages / Last contacted and
  the source. `Settings.tsx`: Remove cached contacts for IMAP accounts too (different wording).
- Decisions: Bcc of a message is never known, so it never counts. Imports go into the account chosen in the filter,
  else the first Gmail (app password) account. Block sender / Create rule stay in the menu for IMAP accounts: rules
  for those accounts are local (existing behaviour).
- Tests changed because the old limitation went away (noted as the task asked): `contacts_tests.rs` no longer expects
  "no suggestions for IMAP accounts" / `remove_cached_for(gmail)` to fail; `ContactsView.test.tsx` "says contacts need
  a Microsoft 365 account" now uses a Google-sign-in account; `Mailbox.test.tsx` likewise; `Settings.test.tsx` expects
  the Remove cached contacts button for the IMAP account too.
- Counts: Rust 83 (+8), frontend 88 (+5 ContactsView, +1 Mailbox). tsc clean.
- Needs a real-world check: a real Gmail mailbox's sender mix (how well `is_automated` separates machines from
  people; the list is deliberately conservative: `info@`/`support@` count as people); a real Google Contacts export
  (both layouts were written from memory of the format); vCards from iOS / Android / Outlook.
