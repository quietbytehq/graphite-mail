import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/** What the organisation's administrator is asked to allow, in plain words. */
export const REQUESTED_PERMISSIONS: { name: string; why: string }[] = [
  { name: "User.Read", why: "your name and email address" },
  { name: "Mail.ReadWrite, Mail.Send", why: "read, file, flag and send your mail" },
  { name: "Calendars.ReadWrite", why: "your calendar and invitations" },
  { name: "MailboxSettings.ReadWrite", why: "Outlook rules (only if you use Rules)" },
  { name: "Contacts.Read, People.Read, User.ReadBasic.All", why: "your contacts and the directory (only if you use Contacts)" },
];

async function copyText(text: string) {
  try { await navigator.clipboard.writeText(text); return true; } catch {
    const t = document.createElement("textarea");
    t.value = text; document.body.appendChild(t); t.select();
    const ok = document.execCommand("copy"); t.remove(); return ok;
  }
}

/** Shown instead of a bare error when Microsoft says the organisation must approve Graphite Mail. */
export default function AdminApproval({ message, email }: { message: string; email?: string }) {
  const [note, setNote] = useState("");
  async function copyLink() {
    setNote("");
    try {
      const url = await invoke<string>("admin_consent_link", { email: email ?? null });
      setNote((await copyText(url)) ? "Link copied. Send it to your IT admin." : `Couldn't copy. Send this link to your IT admin: ${url}`);
    } catch (e) { setNote(String(e)); }
  }
  return (
    <div className="notice admin-approval" role="alert">
      <b>Your organisation needs to approve Graphite Mail</b>
      <p>{message}</p>
      <div className="rowbtns"><button className="primary" onClick={copyLink}>Copy link for your IT admin</button></div>
      {note && <div className="muted" style={{ fontSize: 12, overflowWrap: "anywhere" }}>{note}</div>}
      <details>
        <summary>What Graphite Mail asks for, and why</summary>
        <ul className="muted" style={{ fontSize: 12, margin: "4px 0 0", paddingLeft: 18 }}>
          {REQUESTED_PERMISSIONS.map((p) => <li key={p.name}><b>{p.name}</b>: {p.why}</li>)}
        </ul>
        <div className="muted" style={{ fontSize: 12 }}>Graphite Mail runs on your computer only: nothing is sent anywhere but Microsoft, and your sign-in stays in your system keyring.</div>
      </details>
    </div>
  );
}
