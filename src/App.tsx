import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import Mailbox from "./Mailbox";
import AppPasswordForm from "./AppPasswordForm";
import { DragBar, TrafficLights } from "./WindowControls";
import { applyTheme, cachedChoice } from "./theme";
import { SIGN_IN_CANCELLED, adminApproval, useBrowserSignIn } from "./signIn";
import AdminApproval from "./AdminApproval";
import { dataFolderFrom } from "./paths";
import "./App.css";

type Account = { email: string; display_name: string };
type SetupState = { ready: boolean; default_dir: string };
/** The system keyring (sign-in tokens, app passwords): usable, or what is wrong and the command that fixes it. */
type KeyringStatus = { ok: boolean; message: string; command: string };

// Show the last theme at once; the saved app setting is loaded (and applied) by the mailbox.
applyTheme(cachedChoice());

function SetupWizard({ defaultDir, onDone }: { defaultDir: string; onDone: () => void }) {
  const [dir, setDir] = useState(defaultDir);
  const [error, setError] = useState("");

  async function browse() {
    const picked = await open({ directory: true, defaultPath: dir, title: "Choose where Graphite Mail stores your mail" });
    if (typeof picked === "string") setDir(dataFolderFrom(picked));
  }

  async function confirm() {
    setError("");
    try {
      await invoke("initialize_storage", { path: dir });
      onDone();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <Onboard>
      <h1>Welcome to Graphite Mail</h1>
      <p className="muted">Choose where your offline mail and calendar will be stored. You can move it later.</p>
      <div className="row">
        <input value={dir} onChange={(e) => setDir(e.target.value)} />
        <button onClick={browse}>Browse…</button>
      </div>
      <button className="primary" onClick={confirm}>Continue</button>
      {error && <p className="error">{error}</p>}
    </Onboard>
  );
}

/** Shown instead of a silent failure when the keyring can't be reached: in the strict snap, the one-off
 *  `snap connect` command; elsewhere, the keyring to install. Sign-in stays off until "Check again" succeeds. */
function KeyringNotice({ status, onRetry }: { status: KeyringStatus; onRetry: () => void }) {
  return (
    <div className="keyring" role="alert">
      <div className="error">{status.message}</div>
      {status.command && <pre className="command">{status.command}</pre>}
      <button onClick={onRetry}>Check again</button>
    </div>
  );
}

function Onboard({ children }: { children: React.ReactNode }) {
  return (
    <div className="onboard">
      <DragBar><TrafficLights /></DragBar>
      <div className="card">
        <img className="logo" src="/logo.png" alt="" />
        {children}
      </div>
    </div>
  );
}

export default function App() {
  const [setup, setSetup] = useState<SetupState | null>(null);
  const [account, setAccount] = useState<Account | null>(null);
  const [error, setError] = useState("");
  const [note, setNote] = useState("");
  // The browser sign-in: Cancel sign-in, a time limit, and nothing from a stopped attempt reaches the screen.
  const signIn = useBrowserSignIn<"microsoft" | "google">((n) => { setError(""); setNote(n); });
  const busy = signIn.waiting !== null;
  const [appPassword, setAppPassword] = useState(false);
  const [googleOk, setGoogleOk] = useState<boolean | null>(null);
  useEffect(() => { invoke<boolean>("google_sign_in_available").then(setGoogleOk).catch(() => setGoogleOk(false)); }, []);

  const loadSetup = () => invoke<SetupState>("setup_state").then(setSetup).catch((e) => setError(String(e)));
  // null = the keyring works (or the check itself failed, which a sign-in will report in its own words).
  const [keyring, setKeyring] = useState<KeyringStatus | null>(null);
  const checkKeyring = () => invoke<KeyringStatus | null>("keyring_status").then((s) => setKeyring(s && !s.ok ? s : null)).catch(() => setKeyring(null));

  useEffect(() => {
    loadSetup();
    checkKeyring();
    invoke<Account | null>("current_account").then(setAccount);
  }, []);

  // Tell the shell the first screen is up, so the splash can hand over.
  useEffect(() => {
    if (setup || error) invoke("app_ready").catch(() => {});
  }, [setup, error]);

  function start(kind: "microsoft" | "google") {
    setError(""); setNote("");
    signIn.run(kind, () => invoke<Account>(kind === "google" ? "add_google_account" : "sign_in"), setAccount, (e) => setError(String(e)));
  }

  async function signOut() {
    await invoke("sign_out");
    setAccount(null);
  }

  if (!setup) return <Onboard><p className="muted">{error || "Loading…"}</p></Onboard>;
  if (!setup.ready) return <SetupWizard defaultDir={setup.default_dir} onDone={loadSetup} />;
  if (account) return <Mailbox account={account} onSignOut={signOut} />;
  if (appPassword) return (
    <Onboard>
      <h1>Graphite Mail</h1>
      <AppPasswordForm onAdded={setAccount} onCancel={() => { setNote(""); setAppPassword(false); }} />
    </Onboard>
  );

  return (
    <Onboard>
      <h1>Graphite Mail</h1>
      <p className="muted">Offline-first mail &amp; calendar for Microsoft 365 and Gmail.</p>
      {keyring && <KeyringNotice status={keyring} onRetry={checkKeyring} />}
      <div className="row" style={{ justifyContent: "center" }}>
        <button className="primary" onClick={() => start("microsoft")} disabled={busy || !!keyring}>
          {signIn.waiting === "microsoft" ? "Waiting for browser sign-in…" : "Sign in with Microsoft"}
        </button>
        {googleOk && <button onClick={() => start("google")} disabled={busy || !!keyring}>{signIn.waiting === "google" ? "Waiting for browser sign-in…" : "Sign in with Google"}</button>}
      </div>
      {googleOk === false && <p className="muted">Google sign-in coming soon. Use an app password for now.</p>}
      <button className="linkish" onClick={() => { setError(""); setNote(""); setAppPassword(true); }} disabled={busy || !!keyring}>Use Gmail or another email account with an app password</button>
      {busy && (
        <div className="row" style={{ justifyContent: "center", alignItems: "center" }}>
          <span className="muted">Finish signing in in your web browser, then come back here.</span>
          <button onClick={() => signIn.stop(SIGN_IN_CANCELLED)}>Cancel sign-in</button>
        </div>
      )}
      {note && !busy && <p className="muted">{note}</p>}
      {error && (adminApproval(error) ? <AdminApproval message={adminApproval(error)!} /> : <p className="error">{error}</p>)}
    </Onboard>
  );
}
