# Changelog

## 1.12.0 — Other email providers, personal Microsoft accounts, admin approval
- **Other email (IMAP)**: Yahoo, iCloud, Zoho, Fastmail and AOL presets (servers filled in, short help on creating an
  app password for each) and a custom IMAP/SMTP server (host, port, TLS / STARTTLS, user name). Gmail-only behaviour
  (labels, All Mail / Starred views, X-GM extensions) now applies only when the server advertises X-GM-EXT-1. Folder
  tiles come from SPECIAL-USE or, on non-Gmail servers, from the usual folder names. Mail only: the provider's
  calendar and address book aren't synced, and the form says so.
- **Personal Microsoft accounts** (outlook.com, hotmail.com, live.com): recognised from the sign-in token; no
  organisation directory is asked for or shown, and the admin-approval message never appears for them.
- **Admin approval**: when an organisation must approve the app first (AADSTS90094, AADSTS65001, "Need admin
  approval"), the first-run screen, Settings and Contacts explain it, with **Copy link for your IT admin** (Microsoft's
  admin-consent page for this app and your tenant) and a note on the permissions requested and why.
- Housekeeping: the data folder path is stored and shown without Windows' `\\?\` prefix after a move (dunce);
  GitHub Actions on Node 24 (checkout v5, setup-node v5, cache v5, upload-artifact v5, download-artifact v6).
- The sidebar probes Microsoft's Conversation History and Sync Issues folders by role (from 1.11.1).

### Security
Hardening after an external review. Each point has tests; `test-fixtures/hostile-mail` holds hostile emails that the
test suite runs through the reader, and `scripts/send-test-mails.py` sends them to a test mailbox of your choice.
- **Email HTML**: message bodies and calendar descriptions are shown only in a sandboxed frame with
  `sandbox="allow-popups"` — no scripts and, new, no `allow-same-origin` (the frame used to share the app's origin).
  HTML is cleaned with DOMPurify (pinned to 3.4.17): scripts, event-handler attributes, forms and form controls,
  iframes/objects/embeds, `meta` (refresh), `base`, `link`, SVG/MathML and `javascript:`/`data:`/`file:` addresses are
  removed, and a frame Content-Security-Policy blocks anything left. Replies and forwards no longer put the original
  message into the editor as an HTML string (it is inserted as a sanitised fragment, the "… wrote:" line as plain text,
  which also fixes markup in a sender's name), and calendar descriptions no longer use `dangerouslySetInnerHTML`.
- **Remote images** are blocked by default, with a placeholder bar per message: **Load images** (this message) or
  **Always load from** the sender (undo with **Stop loading them**). Pictures inside the message (`cid:`, `data:`) still
  show. Remote style sheets and fonts are never loaded.
- **Links**: every link in a message asks for a new window, whatever its `target`; the app refuses the window and only
  acts on `http`/`https` (system browser) and `mailto` (new message). Everything else does nothing. The windows can't
  navigate away from the app: other hosts on the app's scheme and `ipc:`/`asset:` are now refused too, and the splash
  window has the same navigation handler.
- **Attachments**: file names lose path separators, `..`, control and invisible (right-to-left override) characters
  and Windows device names (`CON`, `COM¹`, `CONIN$` …); saving always writes directly into the chosen folder (no links
  followed, mode 0600, marked as downloaded on Windows). Programs, scripts, installers, disk images, shortcuts,
  `.desktop` files and files without an extension are only saved, never opened from the app. Opened copies live in a
  private per-user folder instead of `/tmp`.
- **Permissions**: the app grants only the Tauri permissions its UI uses (no webview/window creation, always-on-top,
  notification, tray, opener or devtools permissions, and no conflicting allow/deny pairs); `dialog:allow-ask`
  (deprecated alias) became `dialog:allow-message`. Developer tools are off in release builds.
- **Data folder** (Linux): folders are 0700 and files 0600 — database, messages, attachments and the settings
  pointer. Folders created by earlier versions are fixed at start-up.
- Tests: a flaky check (a random Message-ID could contain "bcc") no longer fails about one run in a hundred.

## 1.11.1 — Sidebar: matte tiles
- New sidebar look ("Option B"): every folder has a flat coloured 24 px tile with a white glyph (Inbox blue, Drafts
  ochre, Sent green, Archive purple, Deleted red, Junk orange, Outbox teal, Sync Issues grey, everything else the
  brown folder tile), 280 px wide, 34 px rows, subfolders indented 22 px. All Inboxes has a larger tile.
- Master Calendar and Contacts are two cards side by side (38 px tile, label, "All calendars" / "People and directory");
  the active view's card uses the theme's selected colour. Below 1100 px the sidebar narrows and the cards stack.
- Tiles are chosen by the folder's role (Microsoft well-known name, Gmail system label, IMAP SPECIAL-USE flag), never
  by its name, so renamed and translated folders keep the right one. Conversation History and Sync Issues (with its
  subfolders) are now recognised. The same tiles appear in the Move menu and the Move/Copy folder picker.
- Fixed: the first launch on Windows failed with "Access is denied" (the settings pointer's directory fsync is now
  Unix-only); Linux and Windows tests run independently in CI.

## 1.11.0 — Windows
- Graphite Mail now builds and runs on **Windows 10/11** (64-bit): an NSIS installer that installs for the current user
  (no administrator rights), with the app icon, and that fetches the WebView2 runtime if the computer doesn't have it.
- Secrets go to the **Windows Credential Manager** (Secret Service / GNOME Keyring / KWallet on Linux, as before).
- Mail data defaults to `%LOCALAPPDATA%\graphite-mail` on Windows (`~/.local/share/graphite-mail` on Linux); the
  settings pointer lives in `%APPDATA%\graphite-mail`. The folder picker builds Windows paths with backslashes.
- Free-space check before moving the data folder works on Windows (`GetDiskFreeSpaceExW`) as well as Linux (`statvfs`).
- Attachment file names are made safe for Windows too (`< > : " | ? *`, trailing dots/spaces, device names like CON).
- Browser sign-in (Microsoft and Google) uses the same loopback redirect on Windows; links, attachments and
  notifications go through Tauri's cross-platform plugins (no `xdg-open`, no D-Bus calls in the app's own code).
- GitHub Actions: a version tag runs the tests on Linux and Windows, builds the `.deb` and the Windows installer on
  separate runners and attaches both to a **draft** release. The Google client secret is never used there.
- **Contacts for Gmail (app password) accounts**, kept on this computer:
  - **People you've emailed**: built from the From/To/Cc headers of synced mail (name, address, messages exchanged,
    last contacted; newest first), updated after every sync without re-reading old mail. Spam, Bin and drafts are
    ignored, as are no-reply boxes, bounce handlers, notification and newsletter senders and mailing-list plumbing.
  - **Import contacts…**: vCard 2.1/3.0/4.0 (folded lines, quoted-printable, several addresses and phones, ORG/TITLE)
    and Google Contacts CSV (classic and current column layouts, `:::`-separated values, quoted cells; Outlook-style
    columns too), in UTF-8, UTF-16 or Windows-1252. Duplicates merge by address, inside the file and against earlier
    imports. Stored as **Imported**.
  - Both groups work with the right-click menu, favourites, vCard export and To/Cc/Bcc suggestions. A note says
    "Google contacts need Google sign-in, coming in a later update."
  - **Remove cached contacts** now also clears these; removing the account deletes them.

## 1.10.3 — Bcc
- **Bcc** in the editor for every account type. The row is hidden until you press the **Bcc** link beside Cc (it opens
  by itself when a draft already has blind copies). Suggestions from the account's contacts work there like in To/Cc.
  A message with only blind copies can be sent. Drafts keep their Bcc (new `bcc_text` column, added by a migration).
- Blind copies never reach other recipients' headers: Microsoft 365 sends them as `bccRecipients` (plain sends, drafts
  with attachments, replies and forwards alike); Gmail through Google sign-in gets a `Bcc` header that the Gmail API
  turns into envelope recipients and removes before delivery; Gmail with an app password puts them in the SMTP envelope
  only (RCPT TO) and writes no Bcc header at all. Tests against fake Graph, Gmail and SMTP servers prove each path.
- Replies, reply-all and forwards start with an empty Bcc (a received message's blind copies are never known).
- Fixed: closing the editor while its signature was still loading could throw.

## 1.10.2 — Safer menus and first-run sign-in
- Mail list right-click menu: the keyboard never starts on Delete, Block sender or Mark as spam. When the items above
  them are unavailable (several messages selected, or the message still loading) focus starts on the next safe item,
  and if nothing safe is enabled the menu itself takes focus, so the first Enter can never delete anything. Arrow keys
  still reach every item. The same rule applies to the Contacts menu (Block sender).
- First-run screen: **Cancel sign-in** while the browser sign-in waits, a 2-minute time limit with a message, and a
  cancelled or timed-out sign-in can no longer bring a late error or open the mailbox. The sign-in logic is now shared
  with Settings → Accounts (one hook, `src/signIn.ts`), and leaving the screen clears its timer.

## 1.10.1 — Contacts right-click menu
- Right-click a contact (or Shift+F10 / the menu key) for New email, New meeting (contact invited, in that account's
  calendar), Search mail with this person (local search, `with:address` in the search box), Copy email address,
  Copy name and address, Copy phone number, Add to / Remove from favourites, Export as vCard, Block sender and
  Create rule for this sender. Several addresses or phone numbers: a submenu to choose. No address: email items hidden.
- Multi-select in the contact list: Ctrl+click, Shift+click ranges, hover checkboxes, Ctrl+A, Esc. The selection's menu:
  New email to all, New meeting with all, Copy all addresses (semicolon-separated), Export selected as vCard.
- Favourites, stored locally per account (removed with the account), shown in a Favourites group at the top.
- vCard 3.0 export (.vcf): N, FN, ORG, TITLE, every EMAIL and TEL, escaped and folded as RFC 2426 says.
- The shared right-click menu gained submenus (→ / Enter / hover to open, ← / Esc to go back) and stays inside the window.
- No new Microsoft Graph permissions.

## 1.10.0 — Contacts (Microsoft 365)
- New **Contacts** view (View → Contacts, or the sidebar): **Directory** (Global Address List) and **My contacts**, each
  with a count and foldable; one search across both; account filter with several Microsoft 365 accounts; details pane with
  addresses, phones, job title, department, office, company, **New email** and **Copy email**.
- To/Cc suggestions in the editor from the sending account's contacts (Microsoft 365 accounts).
- Sync: personal contacts of every contact folder through Graph delta queries (full listing where delta isn't offered,
  restart when a sync point expires); the directory through `/users`, paged (999 per page), with a pause between pages,
  `Retry-After` honoured on 429/503, and entries that are gone removed after each full listing; `/me/people` fills in titles
  and phone numbers that basic directory access can't read. Disabled accounts and entries without a mailbox are skipped.
- **New permissions**, asked for only on **Grant access**: `Contacts.Read`, `People.Read`, `User.ReadBasic.All`.
  If they are missing or the organisation blocks them, the view says so and offers Grant access again. Mail and calendar
  sync never depend on them (a refresh that fails with them falls back to the mail/calendar permissions).
- Privacy: contacts stay in the local database only and are never logged; **Remove cached contacts** per account
  (Settings); removing an account deletes its contacts.

## 1.9.1 — Settings → Accounts fixes
- App-password form toggles, scrolls into view and focuses the address field; readable text boxes in all themes;
  Connect explains why it's inactive; browser sign-in can be cancelled and times out after 2 minutes; builds without the
  Google client secret say "Google sign-in coming soon".

## 1.9.0 — Gmail with an app password
- IMAP/SMTP account type for @gmail.com with a Google app password (no OAuth client needed).

Earlier versions: see [PLAN.md](PLAN.md).
