import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ask, open } from "@tauri-apps/plugin-dialog";
import { THEMES, ThemeChoice } from "./theme";
import AppPasswordForm from "./AppPasswordForm";
import { Acct, providerLabel } from "./accounts";
import { SIGN_IN_CANCELLED, adminApproval, useBrowserSignIn } from "./signIn";
import AdminApproval from "./AdminApproval";
import { dataFolderFrom } from "./paths";

/** Small preview colours for each theme: sidebar, content, accent. */
const SWATCH: Record<string, [string, string, string]> = {
  white: ["#f6f6f7", "#ffffff", "#0a7cff"], lightgrey: ["#dfe3ea", "#ffffff", "#0a7cff"], mediumgrey: ["#a3a7ad", "#c9ccd1", "#1f5fbf"],
  darkgrey: ["#28282e", "#1f1f23", "#0a84ff"], black: ["#000000", "#111113", "#0a84ff"], colourful: ["#12352b", "#ffffff", "#c9a227"],
};

type Info = { path: string; size_bytes: number };
const PALETTE = ["#0a84ff", "#30d158", "#ff9f0a", "#bf5af2", "#ff375f", "#64d2ff"];
type Progress = { phase: number; label: string; done: number; total: number };

const fmtSize = (b: number) => (b > 1 << 30 ? `${(b / (1 << 30)).toFixed(1)} GB` : `${Math.max(1, Math.round(b / (1 << 20)))} MB`);

export default function Settings({ accounts, onClose, onAccountsChanged, theme, onTheme }: {
  accounts: Acct[]; onClose: () => void; onAccountsChanged: () => void; theme: ThemeChoice; onTheme: (t: ThemeChoice) => void;
}) {
  const [appPassword, setAppPassword] = useState(false);
  // null until known; false in builds made without the Google client secret.
  const [googleOk, setGoogleOk] = useState<boolean | null>(null);
  useEffect(() => { invoke<boolean>("google_sign_in_available").then(setGoogleOk).catch(() => setGoogleOk(false)); }, []);
  const [info, setInfo] = useState<Info | null>(null);
  const [moving, setMoving] = useState<Progress | null>(null);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [notifs, setNotifs] = useState(true);
  const [sigs, setSigs] = useState<Record<string, string>>({});
  const [sigSaved, setSigSaved] = useState<string>("");
  useEffect(() => {
    accounts.forEach((a) => invoke<string>("get_signature", { email: a.email }).then((t) => setSigs((m) => ({ ...m, [a.email]: t }))).catch(() => {}));
  }, [accounts.map((a) => a.email).join(",")]);
  async function saveSig(email: string) {
    try { await invoke("set_signature", { email, text: sigs[email] ?? "" }); setSigSaved(email); } catch (e) { setError(String(e)); }
  }
  useEffect(() => { invoke<boolean>("get_notifications").then(setNotifs).catch(() => {}); }, []);
  const toggleNotifs = async (on: boolean) => {
    setNotifs(on);
    try { await invoke("set_notifications", { on }); } catch (e) { setError(String(e)); setNotifs(!on); }
  };

  // The browser sign-in: cancellable, times out, and a stopped one can no longer change the screen.
  const signIn = useBrowserSignIn<"microsoft" | "google">((note) => { setError(""); setMessage(note); });
  const adding = signIn.waiting;
  function add(kind: "microsoft" | "google") {
    setError(""); setMessage("");
    signIn.run(kind,
      async () => { const a = await invoke<Acct>(kind === "google" ? "add_google_account" : "sign_in"); onAccountsChanged(); return a; },
      (a) => setMessage(`${a.email} is signed in. Downloading its mail…`),
      (e) => setError(String(e)));
  }
  async function removeContacts(a: Acct) {
    setError(""); setMessage("");
    try {
      await invoke("remove_cached_contacts", { email: a.email });
      setMessage(a.provider === "imap"
        ? `People you've emailed and imported contacts of ${a.email} were removed. Contacts → Refresh contacts rebuilds the people list from your mail; imported contacts need importing again.`
        : `Cached contacts of ${a.email} were removed. Contacts → Refresh contacts downloads them again.`);
    }
    catch (e) { setError(String(e)); }
  }
  async function remove(a: Acct) {
    setError(""); setMessage("");
    const yes = await ask(`Remove ${a.email}?\n\nIts downloaded mail and calendar are deleted from this computer. Nothing is deleted on the server, and you can add the account again any time.`, { title: "Remove account", kind: "warning", okLabel: "Remove", cancelLabel: "Cancel" });
    if (!yes) return;
    try { await invoke("remove_account", { email: a.email }); setMessage(`${a.email} was removed.`); onAccountsChanged(); }
    catch (e) { setError(String(e)); }
  }

  const load = () => invoke<Info>("data_folder_info").then(setInfo).catch((e) => setError(String(e)));
  useEffect(() => {
    load();
    const un = listen<Progress>("migration-progress", (e) => setMoving(e.payload));
    return () => { un.then((f) => f()); };
  }, []);

  async function move() {
    setError(""); setMessage("");
    const picked = await open({ directory: true, title: "Choose where Graphite Mail should keep your mail" });
    if (typeof picked !== "string") return;
    const target = dataFolderFrom(picked);
    const yes = await ask(
      `Move your mail and calendar data to:\n${target}\n\nThis can take a few minutes for a large mailbox. Your current data stays untouched until the copy has been checked.`,
      { title: "Move data folder", kind: "info", okLabel: "Move", cancelLabel: "Cancel" },
    );
    if (!yes) return;
    setMoving({ phase: 1, label: "Starting…", done: 0, total: 1 });
    try {
      const where = await invoke<string>("move_data_folder", { target });
      setMessage(`Done. Your data is now in ${where}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setMoving(null);
      load();
    }
  }

  const pct = moving ? Math.round(((moving.phase - 1 + (moving.total ? moving.done / moving.total : 0)) / 7) * 100) : 0;

  return (
    <div className="sheet-backdrop" onClick={() => !moving && onClose()}>
      <div className="sheet evsheet" onClick={(e) => e.stopPropagation()}>
        <div className="sheet-bar"><span /><b>Settings</b><button disabled={!!moving} onClick={onClose}>Close</button></div>
        <div className="evbody">
          <div className="muted" style={{ fontSize: 11, fontWeight: 600 }}>ACCOUNTS</div>
          {accounts.map((a, i) => (
            <div key={a.email} className="acctrow">
              <i className="acctdot" style={{ background: PALETTE[i % PALETTE.length] }} />
              <div className="acct-name"><b>{a.display_name}</b><span className="muted">{a.email} · {providerLabel(a.provider, a.personal, a.imap)}</span></div>
              {(a.provider ?? "microsoft") !== "google" && (
                <button disabled={!!moving} title={a.provider === "imap" ? "Delete this account's people-you've-emailed list and imported contacts from this computer" : "Delete this account's downloaded contacts from this computer"} onClick={() => removeContacts(a)}>Remove cached contacts</button>
              )}
              <button disabled={!!moving} onClick={() => remove(a)}>Remove</button>
            </div>
          ))}
          <div className="rowbtns">
            <button disabled={!!adding || !!moving} onClick={() => add("microsoft")}>{adding === "microsoft" ? "Waiting for browser…" : "Add Microsoft account"}</button>
            {googleOk && <button disabled={!!adding || !!moving} onClick={() => add("google")}>{adding === "google" ? "Waiting for browser…" : "Add Google account"}</button>}
            <button className={appPassword ? "toggled" : undefined} aria-expanded={appPassword} aria-controls="apppw-form" disabled={!!moving}
              onClick={() => { setError(""); setMessage(""); setAppPassword((open) => !open); }}>Add Gmail or other email (app password)</button>
          </div>
          {googleOk === false && <div className="muted" style={{ fontSize: 12 }}>Google sign-in coming soon. Use an app password for now.</div>}
          {adding && (
            <div className="rowbtns" style={{ alignItems: "center" }}>
              <span className="muted" style={{ fontSize: 12 }}>Finish signing in in your web browser, then come back here.</span>
              <button onClick={() => signIn.stop(SIGN_IN_CANCELLED)}>Cancel sign-in</button>
            </div>
          )}
          {appPassword && (
            <div className="rulecard" id="apppw-form">
              <AppPasswordForm
                onAdded={(a) => { setAppPassword(false); setMessage(`${a.email} is connected. Downloading its mail…`); onAccountsChanged(); }}
                onCancel={() => setAppPassword(false)} />
            </div>
          )}
          <div className="muted" style={{ fontSize: 11, fontWeight: 600, marginTop: 6 }}>SIGNATURES</div>
          <div className="muted" style={{ fontSize: 12 }}>Each account has its own signature. It is added to new messages, replies and forwards sent from that account.</div>
          {accounts.map((a) => (
            <div key={a.email} className="rulecard">
              <div className="rulehead"><b>{a.display_name}</b><span className="muted" style={{ fontSize: 12 }}>{a.email}</span></div>
              <textarea rows={3} placeholder={"Best regards,\nYour name"} value={sigs[a.email] ?? ""}
                onChange={(e) => { setSigs((m) => ({ ...m, [a.email]: e.target.value })); setSigSaved(""); }}
                style={{ width: "100%", resize: "vertical", userSelect: "text" }} />
              <div className="rowbtns" style={{ alignItems: "center" }}>
                <button onClick={() => saveSig(a.email)}>Save signature</button>
                {sigSaved === a.email && <span style={{ color: "#30a46c", fontSize: 12.5 }}>Saved.</span>}
              </div>
            </div>
          ))}
          <div className="muted" style={{ fontSize: 11, fontWeight: 600, marginTop: 6 }}>APPEARANCE</div>
          <div className="themegrid" role="radiogroup" aria-label="Theme">
            <button role="radio" aria-checked={theme === "system"} className={`themeopt ${theme === "system" ? "on" : ""}`} onClick={() => onTheme("system")}>
              <span className="swatch"><i style={{ background: "#ffffff" }} /><i style={{ background: "#1f1f23" }} /></span>Match system
            </button>
            {THEMES.map((t) => (
              <button key={t.id} role="radio" aria-checked={theme === t.id} className={`themeopt ${theme === t.id ? "on" : ""}`} onClick={() => onTheme(t.id)}>
                <span className="swatch">{SWATCH[t.id].map((c, i) => <i key={i} style={{ background: c }} />)}</span>{t.label}
              </button>
            ))}
          </div>
          <div className="muted" style={{ fontSize: 11, fontWeight: 600, marginTop: 6 }}>NOTIFICATIONS</div>
          <label className="switchrow">
            <span>Show a desktop notification when new mail arrives in the Inbox</span>
            <input type="checkbox" className="switch" checked={notifs} onChange={(e) => toggleNotifs(e.target.checked)} />
          </label>
          <div className="muted" style={{ fontSize: 11, fontWeight: 600, marginTop: 6 }}>DATA FOLDER</div>
          <div style={{ userSelect: "text", overflowWrap: "anywhere" }}>{info?.path ?? "…"}</div>
          {info && <div className="muted">{fmtSize(info.size_bytes)} used</div>}
          {moving ? (
            <div>
              <div>{moving.label} <span className="muted">(step {moving.phase} of 7)</span></div>
              <div className="progress"><div style={{ width: `${pct}%` }} /></div>
              <div className="muted" style={{ fontSize: 12 }}>Please keep Graphite Mail open. Your mail is safe even if the computer switches off.</div>
            </div>
          ) : (
            <div><button onClick={move}>Move to another folder…</button></div>
          )}
          {message && <div style={{ color: "#30a46c" }}>{message}</div>}
          {error && (adminApproval(error) ? <AdminApproval message={adminApproval(error)!} /> : <div className="error">{error}</div>)}
        </div>
      </div>
    </div>
  );
}
