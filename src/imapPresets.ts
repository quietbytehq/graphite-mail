/** Servers and app-password help for the email providers "Other email (IMAP)" knows, plus a custom server. */
export type TlsMode = "tls" | "starttls" | "none";
export type ImapSettings = {
  preset: string; imap_host: string; imap_port: number; imap_tls: TlsMode; smtp_host: string; smtp_port: number; smtp_tls: TlsMode; username: string;
};
export type Preset = {
  id: string; name: string; addressHint: string;
  /** Servers; absent for Gmail (built into the app) and for "custom" (typed in). */
  servers?: Omit<ImapSettings, "preset" | "username">;
  /** How to create an app password there, one short line each, and the page to open. */
  help: string[]; helpUrl?: string;
};

export const PRESETS: Preset[] = [
  { id: "gmail", name: "Gmail", addressHint: "name@gmail.com",
    help: ["Your Google account needs 2-Step Verification switched on.", "Create an app password at myaccount.google.com/apppasswords (any name will do).", "Paste the 16-letter code below; spaces are removed for you."],
    helpUrl: "https://myaccount.google.com/apppasswords" },
  { id: "yahoo", name: "Yahoo Mail", addressHint: "name@yahoo.com",
    servers: { imap_host: "imap.mail.yahoo.com", imap_port: 993, imap_tls: "tls", smtp_host: "smtp.mail.yahoo.com", smtp_port: 465, smtp_tls: "tls" },
    help: ["Open Yahoo's Account security page and choose Generate app password (or Manage app passwords).", "Name it, for example “Graphite Mail”, and copy the password Yahoo shows.", "Yahoo app passwords work with AT&T, Rogers and other Yahoo-hosted addresses too."],
    helpUrl: "https://login.yahoo.com/account/security" },
  { id: "icloud", name: "iCloud Mail", addressHint: "name@icloud.com",
    servers: { imap_host: "imap.mail.me.com", imap_port: 993, imap_tls: "tls", smtp_host: "smtp.mail.me.com", smtp_port: 587, smtp_tls: "starttls" },
    help: ["Sign in at account.apple.com → Sign-In and Security → App-Specific Passwords.", "Choose Generate an app-specific password, name it, and copy it.", "Use your full iCloud address (name@icloud.com, @me.com or @mac.com) as the address."],
    helpUrl: "https://account.apple.com/account/manage" },
  { id: "zoho", name: "Zoho Mail", addressHint: "name@zohomail.com",
    servers: { imap_host: "imap.zoho.com", imap_port: 993, imap_tls: "tls", smtp_host: "smtp.zoho.com", smtp_port: 465, smtp_tls: "tls" },
    help: ["In Zoho Mail turn on IMAP access (Settings → Mail accounts → IMAP).", "Then at accounts.zoho.com → Security → App Passwords, generate one (two-factor authentication must be on).", "Accounts in the EU, India or Australia use imap.zoho.eu / .in / .com.au: choose Custom server for those."],
    helpUrl: "https://accounts.zoho.com/home#security/apppassword" },
  { id: "fastmail", name: "Fastmail", addressHint: "name@fastmail.com",
    servers: { imap_host: "imap.fastmail.com", imap_port: 993, imap_tls: "tls", smtp_host: "smtp.fastmail.com", smtp_port: 465, smtp_tls: "tls" },
    help: ["In Fastmail open Settings → Privacy & Security → Integrations (Third-party apps) → New app password.", "Pick “Mail (IMAP/POP/SMTP)” as the access, and copy the password shown."],
    helpUrl: "https://app.fastmail.com/settings/security/devices" },
  { id: "aol", name: "AOL Mail", addressHint: "name@aol.com",
    servers: { imap_host: "imap.aol.com", imap_port: 993, imap_tls: "tls", smtp_host: "smtp.aol.com", smtp_port: 465, smtp_tls: "tls" },
    help: ["Open AOL's Account security page and choose Generate app password.", "Name it and copy the password AOL shows."],
    helpUrl: "https://login.aol.com/account/security" },
  { id: "custom", name: "Custom server", addressHint: "name@example.com",
    help: ["Type the IMAP and SMTP servers your provider documents, with the port and whether they use TLS (usually 993 and 465) or STARTTLS (143 and 587).", "If your provider offers app passwords, use one; otherwise your normal password works only where the provider allows it."] },
];

export const presetById = (id: string) => PRESETS.find((p) => p.id === id) ?? PRESETS[0];

/** The settings handed to the backend for a preset (null for Gmail, whose servers are built in). */
export function settingsFor(preset: string, custom: Omit<ImapSettings, "preset" | "username">, username: string): ImapSettings | null {
  if (preset === "gmail") return null;
  const servers = preset === "custom" ? custom : presetById(preset).servers!;
  return { preset, ...servers, username: username.trim() };
}

/** The label Settings shows for an IMAP account. */
export function imapLabel(settings?: { preset?: string } | null) {
  if (!settings) return "Gmail (app password)";
  const p = PRESETS.find((x) => x.id === settings.preset);
  return p && p.id !== "custom" ? `${p.name} (IMAP)` : "Other email (IMAP)";
}
