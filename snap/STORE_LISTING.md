# Snap Store listing for `graphite-mail`

Paste these into the store page at <https://snapcraft.io/graphite-mail/listing> (publisher: Quiet Byte). The
summary, description, license, website and links are also in `snapcraft.yaml`, so each upload carries them; the store
page keeps what you set here (categories, screenshots and the banner are store-only).

## Title

Graphite Mail

## Summary (69 characters, the store allows 79)

Offline-first mail, calendar and contacts for Microsoft 365 and Gmail

## Description

Graphite Mail is a free, open-source mail, calendar and contacts client for Microsoft 365, personal Microsoft accounts (outlook.com, hotmail.com, live.com), Gmail and any IMAP provider (Yahoo, iCloud, Zoho, Fastmail, AOL or a custom server).

Your mailbox is stored on this computer, in a folder you choose: read, search, reply, file and plan while offline, and your changes are sent when you reconnect, in order, with retries. Several accounts at once with an All Inboxes view, a calendar with invitations and Teams / Meet meetings, your organisation's directory, Outlook rules and Gmail filters, Bcc, signatures, attachments, drafts, six themes and keyboard shortcuts.

No server of its own, no telemetry: the app talks to your provider directly. Sign-in tokens and app passwords live only in your system keyring (GNOME Keyring or KWallet). Because the snap is strictly confined, give it access to the keyring once:

    sudo snap connect graphite-mail:password-manager-service

Email HTML is shown in a script-less sandbox, remote images are blocked until you allow them, and links open in your web browser.

**Signing in.** Microsoft 365 and personal Microsoft accounts sign in through your browser. Gmail and the other providers use an app password (the app shows where to create one). Google sign-in is not in this build.

**Where your mail is.** `~/snap/graphite-mail/common/graphite-mail` unless you choose another folder on the first start; you can move it later from Settings. To keep it on another drive, also run `sudo snap connect graphite-mail:removable-media`.

Free and open source (Apache-2.0). Source, user guide and issues: https://github.com/quietbytehq/graphite-mail

## Categories

- Primary: **Productivity**
- Secondary: **Office**

## Links

| Field | Value |
|---|---|
| Website | https://github.com/quietbytehq/graphite-mail |
| Source code | https://github.com/quietbytehq/graphite-mail |
| Issues / bug tracker | https://github.com/quietbytehq/graphite-mail/issues |
| Contact | https://github.com/quietbytehq/graphite-mail/issues |
| License | Apache-2.0 |

## Icon

`snap/gui/graphite-mail.png` (256 × 256, the app icon; the store asks for 256 × 256 or larger, under 256 KB).

## Screenshots (five, all 1280 × 800 PNG, from `docs/screenshots/`)

Upload in this order; the first one is the one people see in search results.

| # | File | Caption |
|---|---|---|
| 1 | `docs/screenshots/mail-white.png` | Mail: folders, message list and reader, white theme |
| 2 | `docs/screenshots/calendar-darkgrey.png` | Calendar with invitations and meetings, dark grey theme |
| 3 | `docs/screenshots/contacts-white.png` | Contacts: your organisation's directory and your own contacts |
| 4 | `docs/screenshots/compose-bcc.png` | Writing a message with To, Cc and Bcc |
| 5 | `docs/screenshots/mail-black.png` | Mail in the black theme (six themes, or follow the system) |

Alternatives if the store wants fewer or different ones: `settings-white.png` (Settings: accounts, themes, data
folder), `contacts-gmail-white.png` (Gmail contacts: people you've emailed and imported), `mail-colourful.png`.

## Store request: connect `password-manager-service` at install

Post this on <https://forum.snapcraft.io/c/store-requests> (category "store requests") once the first stable
revision is published. Title and body:

**Title:** Auto-connect request: graphite-mail password-manager-service

**Body:**

> Hello, I'm the publisher of the snap `graphite-mail` (Quiet Byte), a free, open-source, offline-first mail, calendar
> and contacts client for Microsoft 365, Gmail and IMAP providers (source: https://github.com/quietbytehq/graphite-mail,
> Apache-2.0, strictly confined, core24, GNOME extension).
>
> I'd like to request auto-connection of the `password-manager-service` interface for `graphite-mail`.
>
> Why the app needs it: Graphite Mail is a mail client, so it must keep OAuth refresh tokens (Microsoft sign-in) and
> provider app passwords (Gmail, Yahoo, iCloud, Zoho, Fastmail, AOL, custom IMAP) somewhere. By design it stores them
> only in the user's Secret Service keyring (GNOME Keyring / KWallet, through the `keyring` Rust crate), never in a
> file or its database. Without the interface the app cannot sign in at all: it detects the missing keyring at start
> and shows the `sudo snap connect graphite-mail:password-manager-service` command on its sign-in screen, which is a
> poor first experience for a store install, and users on a desktop without a terminal habit will assume the app is
> broken. Storing secrets in the keyring is the safer choice for users compared with the fallback other mail clients
> use (an encrypted file under the snap's own home), and it is the only use the snap makes of the interface: it reads
> and writes its own entries (service "graphite-mail") and nothing else. The app is a client for the user's own
> accounts; it has no server of its own and no telemetry.
>
> The snap also plugs `removable-media` (so the mail folder can live on another drive); I am not asking for that one to
> be auto-connected, the app's README explains the manual connection.
>
> Thank you.

## One-time: store credentials for GitHub Actions

The release workflow uploads with the repository secret `SNAPCRAFT_STORE_CREDENTIALS`. To make or renew it, on a
computer with snapcraft (`sudo snap install snapcraft --classic`):

```
snapcraft login
snapcraft export-login --snaps graphite-mail \
  --acls package_access,package_push,package_update,package_release \
  --expires "2027-10-01T00:00:00" credentials.txt
```

Paste the whole file into GitHub → Settings → Secrets and variables → Actions → `SNAPCRAFT_STORE_CREDENTIALS`, then
delete the file (`shred -u credentials.txt`). The login expires on the date given; put a reminder in your calendar.
