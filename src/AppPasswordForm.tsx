import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Acct, cleanAppPassword, connectHint } from "./accounts";
import { PRESETS, TlsMode, presetById, settingsFor } from "./imapPresets";

const TLS_MODES: { id: TlsMode; label: string }[] = [{ id: "tls", label: "TLS" }, { id: "starttls", label: "STARTTLS" }, { id: "none", label: "None" }];

/** Adds Gmail, or another email account over IMAP/SMTP, with an app password. Both servers are checked before
 *  anything is saved. It scrolls itself into view and puts the cursor in the address field when it opens. */
export default function AppPasswordForm({ onAdded, onCancel }: { onAdded: (a: Acct) => void; onCancel?: () => void }) {
  const [preset, setPreset] = useState("gmail");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [username, setUsername] = useState("");
  const [custom, setCustom] = useState({ imap_host: "", imap_port: 993, imap_tls: "tls" as TlsMode, smtp_host: "", smtp_port: 465, smtp_tls: "tls" as TlsMode });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const form = useRef<HTMLFormElement>(null);
  const emailField = useRef<HTMLInputElement>(null);
  useEffect(() => {
    form.current?.scrollIntoView?.({ block: "nearest", behavior: "smooth" });
    emailField.current?.focus({ preventScroll: true });
  }, []);
  const p = presetById(preset);
  const gmail = preset === "gmail";
  const code = gmail ? cleanAppPassword(password) : password.trim();
  const customMissing = preset === "custom" && (!custom.imap_host.trim() || !custom.smtp_host.trim()) ? "Enter the IMAP and SMTP server names" : null;
  const hint = connectHint(email, password, gmail) ?? customMissing;
  const ready = hint === null;

  async function connect(e: React.FormEvent) {
    e.preventDefault();
    if (!ready || busy) return;
    setBusy(true); setError("");
    try {
      const a = await invoke<Acct>("add_imap_account", { email: email.trim(), password: code, settings: settingsFor(preset, custom, username) });
      setPassword("");
      onAdded(a);
    } catch (err) { setError(String(err)); }
    finally { setBusy(false); }
  }
  const openHelp = (ev: React.MouseEvent) => { ev.preventDefault(); if (p.helpUrl) invoke("open_link", { url: p.helpUrl }).catch(() => {}); };
  const field = <K extends keyof typeof custom>(k: K, v: (typeof custom)[K]) => setCustom((c) => ({ ...c, [k]: v }));

  return (
    <form ref={form} className="form apppw" onSubmit={connect} aria-label="Email with an app password">
      <b>{gmail ? "Gmail with an app password" : `${p.name} with an app password`}</b>
      <label>Email provider
        <select value={preset} disabled={busy} onChange={(e) => { setPreset(e.target.value); setError(""); }}>
          {PRESETS.map((x) => <option key={x.id} value={x.id}>{x.name}</option>)}
        </select>
      </label>
      <ol className="muted apppw-help" aria-label="How to create an app password">
        {p.help.map((line, i) => <li key={i}>{line}{i === 0 && p.helpUrl && <> <a href={p.helpUrl} onClick={openHelp}>Open the page</a></>}</li>)}
      </ol>
      {p.servers && (
        <div className="muted" style={{ fontSize: 12 }}>
          Servers: {p.servers.imap_host}:{p.servers.imap_port} ({p.servers.imap_tls.toUpperCase()}) for reading, {p.servers.smtp_host}:{p.servers.smtp_port} ({p.servers.smtp_tls.toUpperCase()}) for sending.
        </div>
      )}
      <label>{gmail ? "Gmail address" : "Email address"}
        <input ref={emailField} type="email" autoComplete="username" placeholder={p.addressHint} value={email} disabled={busy} onChange={(e) => setEmail(e.target.value)} />
      </label>
      <label>App password
        <input type="password" autoComplete="off" spellCheck={false} placeholder={gmail ? "abcd efgh ijkl mnop" : "the app password from your provider"} value={password} disabled={busy}
          onChange={(e) => setPassword(e.target.value)} />
      </label>
      {!gmail && (
        <label>User name <span className="muted">(only if it isn't the address)</span>
          <input type="text" autoComplete="off" spellCheck={false} placeholder={email || p.addressHint} value={username} disabled={busy} onChange={(e) => setUsername(e.target.value)} />
        </label>
      )}
      {preset === "custom" && (
        <div className="apppw-servers">
          {([["IMAP server", "imap_host", "imap_port", "imap_tls"], ["SMTP server", "smtp_host", "smtp_port", "smtp_tls"]] as const).map(([title, host, port, tls]) => (
            <div key={title} className="rowbtns" style={{ alignItems: "flex-end" }}>
              <label style={{ flex: 2 }}>{title}
                <input type="text" spellCheck={false} placeholder={host === "imap_host" ? "imap.example.com" : "smtp.example.com"} value={custom[host]} disabled={busy} onChange={(e) => field(host, e.target.value)} />
              </label>
              <label style={{ width: 80 }}>Port
                <input type="number" min={1} max={65535} value={custom[port]} disabled={busy} onChange={(e) => field(port, Math.max(0, Math.min(65535, Number(e.target.value) || 0)))} />
              </label>
              <label style={{ width: 110 }}>Security
                <select value={custom[tls]} disabled={busy} onChange={(e) => field(tls, e.target.value as TlsMode)}>
                  {TLS_MODES.map((m) => <option key={m.id} value={m.id}>{m.label}</option>)}
                </select>
              </label>
            </div>
          ))}
        </div>
      )}
      <div className="muted" style={{ fontSize: 12 }}>
        The app password is kept in your system keyring, not in Graphite Mail's files. You can revoke it at any time with your provider.
        {!gmail && " This account type is mail only: the provider's calendar and address book aren't synced (people you've emailed and imported contacts still work)."}
      </div>
      <div className="rowbtns">
        <button className="primary" type="submit" disabled={!ready || busy} aria-describedby={hint ? "apppw-hint" : undefined}>{busy ? `Checking with ${gmail ? "Gmail" : "the server"}…` : "Connect"}</button>
        {onCancel && <button type="button" disabled={busy} onClick={onCancel}>Cancel</button>}
      </div>
      {hint && !busy && <div id="apppw-hint" className="muted apppw-hint">{hint}</div>}
      {error && <div className="error" role="alert">{error}</div>}
    </form>
  );
}
