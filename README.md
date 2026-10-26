<p align="center"><img src="docs/logo.png" width="220" alt="Graphite Mail"></p>

# Graphite Mail

A free, open-source, **offline-first mail, calendar and contacts client for Microsoft 365 and Gmail**, for **Linux and Windows**.
Built with Tauri (Rust) + React. Talks to Microsoft Graph, the Gmail / Google Calendar APIs, and (for Gmail with an app
password) IMAP + SMTP. No server of its own, no telemetry: your mail goes from the provider to a folder you choose.

![Mail, white theme](docs/screenshots/mail-white.png)

## Features

- **Microsoft 365 and Gmail**, several accounts at once, with an **All Inboxes** view and a colour dot per account.
- Gmail through **Google sign-in** (needs a build with a Google client, see below) or with a **Google app password**
  (IMAP/SMTP, works in every build).
- **Other email over IMAP**: Yahoo, iCloud, Zoho, Fastmail, AOL (presets with app-password help) or any custom
  IMAP/SMTP server (TLS or STARTTLS). Mail only: these providers' calendars and address books aren't synced.
- **Personal Microsoft accounts** (outlook.com, hotmail.com, live.com) as well as Microsoft 365.
- **Offline first**: your mailbox is stored on this computer; read and search it without a connection. Read, flag, move,
  delete, reply and send while offline: changes queue up and are replayed when you reconnect, in order, with retries.
- **Compose** with To, Cc and **Bcc** (blind copies never reach other recipients' headers on any account type), signatures
  per account, attachments up to 15 MB, drafts saved automatically, replies and forwards that thread correctly.
- **Calendar** for Microsoft 365 and Google accounts: events, invitations (accept / tentative / decline), new events and
  Teams / Meet meetings.
- **Contacts**: for Microsoft 365, your organisation's **Directory** and **My contacts**; for Gmail (app password),
  **People you've emailed** (built from your mail, with message counts) and **Imported** (vCard or Google Contacts CSV).
  Favourites, search, vCard export, To/Cc/Bcc suggestions, and a right-click menu (new email, new meeting, search mail
  with this person, copy, block, create rule).
- **Rules**: Outlook inbox rules and Gmail filters, edited from the app; local rules for app-password accounts.
- **You choose where your mail lives**, and can move it later: a crash-safe, verified copy with one atomic switch.
- A **matte sidebar**: every folder has a flat coloured tile with a white glyph, chosen by the folder's role (so a
  renamed or translated Inbox is still blue), and Master Calendar and Contacts sit in two cards at the top.
- Six **themes** (White, Light Grey, Medium Grey, Dark Grey, Black, Colourful) or follow the system.
- Keyboard-friendly: shortcuts for the common actions, full keyboard access to the right-click menus (which never start
  on a destructive item), Shift+F10 / the menu key.
- Secrets (sign-in tokens, app passwords) live only in your **system keyring**: Secret Service (GNOME Keyring / KWallet)
  on Linux, **Credential Manager** on Windows.

<table>
  <tr>
    <td><img src="docs/screenshots/contacts-white.png" alt="Contacts, white theme"></td>
    <td><img src="docs/screenshots/contacts-gmail-white.png" alt="Gmail contacts: people you've emailed and imported"></td>
  </tr>
  <tr>
    <td><img src="docs/screenshots/mail-context-menu-darkgrey.png" alt="Mail right-click menu, dark grey theme"></td>
    <td><img src="docs/screenshots/contacts-context-menu-white.png" alt="Contacts right-click menu"></td>
  </tr>
  <tr>
    <td><img src="docs/screenshots/compose-bcc.png" alt="Compose with Bcc"></td>
    <td><img src="docs/screenshots/settings-white.png" alt="Settings"></td>
  </tr>
  <tr>
    <td><img src="docs/screenshots/mail-black.png" alt="Mail, black theme"></td>
    <td><img src="docs/screenshots/mail-colourful.png" alt="Mail, colourful theme"></td>
  </tr>
  <tr>
    <td><img src="docs/screenshots/calendar-darkgrey.png" alt="Calendar, dark grey theme"></td>
    <td><img src="docs/screenshots/mail-lightgrey.png" alt="Mail, light grey theme"></td>
  </tr>
</table>

The sidebar in all six themes:

<table>
  <tr>
    <td><img src="docs/screenshots/sidebar-white.png" alt="Sidebar, White"></td>
    <td><img src="docs/screenshots/sidebar-lightgrey.png" alt="Sidebar, Light Grey"></td>
    <td><img src="docs/screenshots/sidebar-mediumgrey.png" alt="Sidebar, Medium Grey"></td>
    <td><img src="docs/screenshots/sidebar-darkgrey.png" alt="Sidebar, Dark Grey"></td>
    <td><img src="docs/screenshots/sidebar-black.png" alt="Sidebar, Black"></td>
    <td><img src="docs/screenshots/sidebar-colourful.png" alt="Sidebar, Colourful"></td>
  </tr>
</table>

More: [`docs/screenshots/`](docs/screenshots/) (contacts in dark grey and colourful, settings in black, the dark context
menus). New to Graphite Mail? Read the **[User Guide](docs/USER-GUIDE.md)**.

## Install

Releases are on the GitHub **Releases** page. Each one has a Linux `.deb`, a Linux `.AppImage` and a Windows installer.
Graphite Mail is also in the **Snap Store** as `graphite-mail`.

### Linux

**Snap** (Ubuntu and any distribution with snapd) — two commands:

```
sudo snap install graphite-mail
sudo snap connect graphite-mail:password-manager-service
```

The second one matters: the snap is strictly confined, and the keyring connection (`password-manager-service`, the
Secret Service that GNOME Keyring and KWallet provide) is not connected automatically. Without it Graphite Mail cannot
save sign-in tokens or app passwords. The app notices and shows that command on its sign-in screen instead of failing
silently; run it, then start Graphite Mail again. One of `gnome-keyring` or `kwalletmanager` must be installed and
running, as for the `.deb`.

What the snap can reach: the network, your home folder (for saving attachments and importing contacts; files and
folders whose names start with a dot are out of bounds, as in every snap), your browser (links and sign-in open there),
notifications, and, once you run `sudo snap connect graphite-mail:removable-media`, drives under `/media` and `/mnt` (to
keep the data folder on another disk).

The snap keeps its mail in its own home: `~/snap/graphite-mail/common/graphite-mail` (chosen on the first start; you can
pick another folder or move it later), with the same private permissions (folders 0700, files 0600). It is not under
`~/snap/graphite-mail/current`, which snapd copies on every update. **Data from the `.deb` or AppImage does not carry
over automatically**: the snap has its own settings pointer and starts with the first-run screen. Signed-in accounts
do carry over, because both use the same keyring entries, so after the first start the snap downloads your mail again.
To reuse the mail already on disk instead: in the old version, move the data folder (Settings → *Move to another
folder…*) to a folder the snap can see, for example `~/Mail/graphite-mail`, then stop using the old version and enter
that folder on the snap's first-run screen (an existing Graphite Mail data folder is adopted as it is). Don't run the
`.deb` and the snap on the same folder at the same time.

Testing builds are on the `edge` channel (`sudo snap install graphite-mail --edge`; back to the release with
`sudo snap refresh graphite-mail --stable`). Remove the snap with `sudo snap remove graphite-mail`: snapd keeps a
snapshot of `~/snap/graphite-mail` (the default data folder included) for 31 days and then deletes it, so copy the
data folder out first if you want to keep it (`--purge` deletes it at once). A data folder you placed elsewhere is
left alone.

**Ubuntu / Debian** — download the `.deb`, then:

```
sudo apt install ./graphite-mail_<version>_amd64.deb
graphite-mail
```

The package recommends `gnome-keyring` or `kwalletmanager`: one of them must be running, because that is where sign-in
tokens are kept. On a minimal desktop install one first.

**Any Linux** — download the `.AppImage`, then:

```
chmod +x graphite-mail_<version>_amd64.AppImage
./graphite-mail_<version>_amd64.AppImage
```

Remove it with `sudo apt remove graphite-mail` (or delete the AppImage). Your mail folder is kept; delete it yourself if
you want it gone (Settings shows where it is).

### Windows (10 / 11, 64-bit)

1. Download `Graphite Mail_<version>_x64-setup.exe` and run it. It installs for the **current user only** (no
   administrator rights needed) under `%LOCALAPPDATA%\Programs\Graphite Mail`, with a Start-menu entry and an uninstaller.
2. If the computer has no **WebView2** runtime (every Windows 11 and most Windows 10 machines already have it), the
   installer downloads it from Microsoft first.
3. The installer isn't code-signed yet, so **SmartScreen** may show "Windows protected your PC". Click *More info* →
   *Run anyway*. You can check the file's hash against the one on the release page first.

Data: settings pointer in `%APPDATA%\graphite-mail`, mail in `%LOCALAPPDATA%\graphite-mail` unless you choose another
folder on the first start. Secrets go to the Windows Credential Manager (Control Panel → Credential Manager → Windows
Credentials, entries named `graphite-mail`). Uninstall from *Settings → Apps*; the mail folder is kept.

## Signing in

- **Microsoft 365**: *Sign in with Microsoft* opens your browser; sign in there and come back. Graphite Mail listens on
  a random port of `localhost` for the redirect, so nothing needs to be configured. *Cancel sign-in* stops it, and it
  gives up by itself after two minutes.
- **Personal Microsoft accounts** (outlook.com, hotmail.com, live.com) sign in the same way. They have no
  organisation directory, so Contacts shows only your own contacts, and no admin approval is ever involved.
- **Gmail with Google sign-in**: the same, with *Sign in with Google* — only in builds made with a Google OAuth client
  (see *Build from source*). The public builds say "Google sign-in coming soon".
- **Gmail with an app password**: works everywhere; see the next section.

## Using Graphite Mail in an organisation

Some organisations don't let people approve apps themselves. Then *Sign in with Microsoft* ends on a "Need admin
approval" page, or Microsoft answers with an error such as AADSTS90094 or AADSTS65001. Graphite Mail recognises
this and shows **Your organisation needs to approve Graphite Mail** with a **Copy link for your IT admin** button.

What to do:
1. Press the button and send the copied link to your IT administrator. It opens Microsoft's admin-consent page for
   Graphite Mail in your organisation's directory (the link holds only the app's public client id and the permission
   names: no tokens, no secrets).
2. The administrator signs in there and approves once. That covers everyone in the organisation.
3. Sign in again in Graphite Mail. (After approving, the administrator's browser is sent to `http://localhost`, which
   shows a "can't connect" page; that is expected and harmless.)

What the link asks for, and why (the same permissions a user would be asked for, see *Permissions* below):
`User.Read` (your name and address), `Mail.ReadWrite` and `Mail.Send` (mail), `Calendars.ReadWrite` (calendar),
`MailboxSettings.ReadWrite` (Outlook rules), `Contacts.Read`, `People.Read` and `User.ReadBasic.All` (contacts and
the directory). The app runs on your computer only and talks to Microsoft directly; there is no Graphite Mail server.
Personal Microsoft accounts (outlook.com, hotmail.com, live.com) never need this.

## How to create a Gmail app password

An app password is a 16-letter code that lets one application read and send your Gmail over IMAP/SMTP without your real
password and without Google sign-in. Google only offers them on accounts with 2-Step Verification.

1. Open **myaccount.google.com → Security** and turn on **2-Step Verification** if it is off.
2. Open **myaccount.google.com/apppasswords** (or search "app passwords" on the Security page). You may be asked to sign
   in again.
3. Type a name, for example `Graphite Mail`, and click **Create**. Google shows a code like `abcd efgh ijkl mnop`.
   Copy it now: Google won't show it again.
4. In Graphite Mail: on the first screen choose **Use Gmail with an app password instead**, or later gear →
   **Add Gmail (app password)**. Enter your Gmail address, paste the code (spaces don't matter) and click **Connect**.
   Graphite Mail checks the code against Gmail's servers before it saves anything.
5. To revoke it later, delete it on the same Google page (and remove the account in Graphite Mail's Settings).

### Other providers (Yahoo, iCloud, Zoho, Fastmail, AOL, custom)

Choose the provider in the same form (*Email provider*). Each preset fills in the servers and shows where that provider
creates app passwords: Yahoo and AOL under *Account security → Generate app password*; iCloud at account.apple.com
→ *Sign-In and Security → App-Specific Passwords*; Zoho at accounts.zoho.com → *Security → App Passwords* (IMAP must be
switched on in Zoho Mail's settings; EU/IN/AU accounts use regional servers, so pick *Custom server* there); Fastmail
under *Settings → Privacy & Security → Integrations → New app password*. *Custom server* takes the IMAP and SMTP host,
port and security (TLS, usually 993/465, or STARTTLS, usually 143/587) and an optional user name. Folder icons come
from the server's SPECIAL-USE flags, or from the usual folder names (Sent, Drafts, Trash, Junk, Archive) where the
server gives none.

Notes: IMAP must be enabled for the account (Gmail → Settings → *See all settings* → *Forwarding and POP/IMAP*; on new
accounts it is). Google Workspace administrators can switch app passwords off for their domain. The code is kept only
in your system keyring, never in a file or the database.

## Permissions

Graphite Mail only ever asks for what a feature needs, when you first use the feature.

| Account type | When | What is asked for | Used for |
|---|---|---|---|
| Microsoft 365 | Sign-in | `openid profile offline_access User.Read Mail.ReadWrite Mail.Send Calendars.ReadWrite` | your name and address, mail (read, flag, move, delete, drafts), sending, calendar |
| Microsoft 365 | Home → Rules → **Allow rules** | `MailboxSettings.ReadWrite` | Outlook inbox rules, Block sender |
| Microsoft 365 | Contacts → **Grant access** | `Contacts.Read People.Read User.ReadBasic.All` | your contacts, the people you work with, the directory (basic profiles) |
| Gmail (Google sign-in) | Sign-in | `openid email profile gmail.modify calendar` | mail, labels, sending, Google Calendar |
| Gmail (Google sign-in) | Home → Rules → **Allow rules** | `gmail.settings.basic` | Gmail filters |
| Gmail (app password) | Adding the account | the app password you created (IMAP + SMTP, TLS) | mail only: no calendar, no Google contacts, local rules |
| Other email (IMAP) | Adding the account | the app password (or password) for that server | mail only: no calendar or provider address book, local rules |

On this computer, the app needs nothing special: it reads and writes its own data folder, keeps secrets in the keyring,
opens links in your browser, shows notifications, and opens a file dialog when you attach, save or import. The snap
spells this out as interfaces (`snap connections graphite-mail`): `network` and `network-bind` (the sign-in redirect on
`localhost`), `desktop`, `desktop-legacy`, `wayland`, `x11`, `opengl`, `home`, `removable-media` and
`password-manager-service`; the last two are the ones you connect by hand. The release builds are made by a public
GitHub Actions workflow and never contain a Google client secret.

Contacts, favourites and the "people you've emailed" list are stored in the local database only, never logged, and are
deleted by **Remove cached contacts** (Settings) or by removing the account.

## Known limits

- **Google sign-in** is not in the public builds (they say "Google sign-in coming soon"). Use an app password, or build
  with your own Google OAuth client.
- **Gmail with an app password** has no calendar and no Google Contacts (the Contacts view says so and offers *People
  you've emailed* and *Import contacts…* instead); its rules run only while Graphite Mail is open; the first sync takes
  the last 90 days of each folder (up to 3000 messages per folder), bigger messages come when opened.
- **Attachments**: 15 MB in total per message. Files that could run code (`.exe`, `.sh`, `.js`, …) can be saved but not
  opened straight from a message.
- **Bcc** copies are, as everywhere, not recorded on the received copies; the sent copy in Microsoft 365 keeps them,
  Gmail's Sent Mail may not show them for app-password accounts.
- **Notifications on Windows** need the installed app (the Start-menu shortcut registers it); running the bare `.exe`
  from a build folder shows none.
- **Plain IMAP servers** (anything but Gmail) have no server-wide message id: a message moved to another folder on
  another device shows up as new after the next sync (read state and flags come from the server, so nothing is lost).
- Shared mailboxes, delegated calendars, S/MIME and PGP are not supported.
- The Windows build is new (v1.11): see the release notes for what has been checked on a real machine.

## Build from source

Needs Node 20+ and a stable Rust toolchain.

**Ubuntu / Debian**: `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev libdbus-1-dev libsecret-1-dev patchelf`.

**Windows**: Visual Studio Build Tools (C++ workload), the Rust `x86_64-pc-windows-msvc` toolchain, **CMake** and **NASM**
on the PATH (the TLS library builds them in), and the WebView2 runtime. Secrets go to the Credential Manager, so nothing
else to install.

```
npm ci
npm run tauri build     # Linux: .deb and .AppImage; Windows: NSIS installer — in src-tauri/target/release/bundle/
npm test                # frontend tests
npx tsc --noEmit        # type check
cd src-tauri && cargo test   # Rust tests (fake Graph, Gmail, IMAP and SMTP servers; no network)
```

The Microsoft sign-in uses the public client id in `src-tauri/src/auth.rs` with the `common` authority. If you register
your own app in Microsoft Entra, set *Supported account types* to **Accounts in any organizational directory and
personal Microsoft accounts** (manifest `signInAudience: AzureADandPersonalMicrosoftAccount`), add the *Mobile and
desktop applications* redirect URI `http://localhost`, and enable *Allow public client flows*. Without the personal
option, outlook.com / hotmail.com / live.com users get AADSTS50020 or AADSTS9002331 at sign-in.

Gmail sign-in needs your own Google OAuth "Desktop app" client. Put it in the untracked file `src-tauri/google_oauth.json`
(copy `src-tauri/google_oauth.example.json`) or set `GRAPHITE_GOOGLE_CLIENT_ID` / `GRAPHITE_GOOGLE_CLIENT_SECRET` when
building. Without it the app builds and works, but *Add Google account* says it isn't set up. **Gmail (app password)**
never uses that client or its secret, so it works in every build.

**Releases**: pushing a tag `vX.Y.Z` runs `.github/workflows/release.yml`: tests on Linux and Windows, the `.deb` and
the Windows installer on separate runners, and a **draft** release with both attached. Publish it by hand after checking
the files. The same tag builds the snap (`snap/snapcraft.yaml`, from source, on an Ubuntu 24.04 runner) and uploads it
to the Snap Store's **stable** channel, which goes live at once; running the workflow by hand (Actions → Release → Run
workflow) uploads the snap to the **edge** channel instead, or only attaches it to the run. The upload uses the
`SNAPCRAFT_STORE_CREDENTIALS` repository secret (made with `snapcraft export-login`). The workflow never has the Google
client secret, so those builds have no Google sign-in, and every build job fails if the string `GOCSPX` is found in
what it built. The store page text is in `snap/STORE_LISTING.md`. To try the snap locally on Ubuntu 24.04:
`sudo snap install snapcraft --classic`, `sudo snapcraft pack --destructive-mode`, then
`sudo snap install --dangerous ./graphite-mail_*.snap` and connect the keyring as above.

## Status

Mail, calendar, contacts, offline changes, data-folder moves, Bcc, local contacts for app-password accounts and the
Windows build are in. See [CHANGELOG.md](CHANGELOG.md) for each version and [PLAN.md](PLAN.md) for the history.
