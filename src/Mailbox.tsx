import React, { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useVirtualizer } from "@tanstack/react-virtual";
import { DragBar } from "./WindowControls";
import Icon from "./Icon";
import Compose, { ComposeInit } from "./Compose";
import CalendarView, { Cal } from "./CalendarView";
import ContactsView from "./ContactsView";
import Settings from "./Settings";
import Rules from "./Rules";
import { Ribbon, RibbonTab, Group, Big, Small, Stack, BigMenu, NamePrompt } from "./Ribbon";
import { ask, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { ContextMenu, FolderPicker, MenuItem, QuickRule, RuleDialog } from "./ContextMenu";
import { Selection, bulkFlag, bulkRead, clickSelect, prune, selectAll, single } from "./selection";
import { THEMES, ThemeChoice, applyTheme, cachedChoice, isChoice, isDark, resolveTheme } from "./theme";
import { isGmail } from "./accounts";
import FolderTile from "./FolderTile";
import MessageFrame from "./MessageFrame";
import { buildDoc } from "./emailHtml";
import { folderRoles } from "./folderIcons";

type Account = { email: string; display_name: string; provider?: string; rules?: boolean };
const ALL = "all-inboxes";
const PALETTE = ["#0a84ff", "#30d158", "#ff9f0a", "#bf5af2", "#ff375f", "#64d2ff"];
type Folder = { id: string; remote_id: string; name: string; well_known: string | null; unread: number; total: number; parent_remote_id: string | null; account_email: string };
type Msg = {
  id: string; subject: string | null; sender_name: string | null; sender_address: string | null;
  snippet: string | null; received_at: number; is_read: boolean; is_flagged: boolean; has_attachments: boolean; account_email: string;
};
type DraftRow = { id: string; mode: ComposeInit["mode"]; reply_to: string | null; from: string | null; to: string; cc: string; bcc?: string; subject: string; html: string; attachments: string[]; updated_at: number };
type Att = { id: string; name: string; mime: string; size: number; downloaded: boolean };
type QueueItem = { seq: number; what: string; failed: boolean; tries: number; error: string | null };
type Detail = Msg & { to_json: string | null; cc_json: string | null; body_html: string | null };

type SyncReport = { messages_changed: number; replay_error: string | null; calendar_error: string | null };
const PAGE = 200;

/** One plain-language line describing how the last sync went. */
function syncStatus(r: SyncReport) {
  if (r.replay_error) return `Couldn't send your changes: ${r.replay_error}`;
  if (r.calendar_error) return `Mail is up to date, but the calendar failed: ${r.calendar_error}`;
  return r.messages_changed ? `Synced · ${r.messages_changed} new` : "Up to date";
}

const fmtBytes = (b: number) => (b >= 1 << 20 ? `${(b / (1 << 20)).toFixed(1)} MB` : `${Math.max(1, Math.round(b / 1024))} KB`);

function fmtDate(ts: number) {
  const d = new Date(ts * 1000);
  const today = new Date();
  return d.toDateString() === today.toDateString()
    ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleDateString([], { day: "numeric", month: "short", year: d.getFullYear() === today.getFullYear() ? undefined : "numeric" });
}

function people(json: string | null) {
  try {
    return (JSON.parse(json || "[]") as { name?: string; address?: string }[]).map((p) => p.name || p.address).join(", ");
  } catch {
    return "";
  }
}

/** Folders of one account as a tree: children under their parent, indented. */
function buildTree(folders: Folder[]) {
  const ids = new Set(folders.map((f) => f.remote_id));
  const kids = new Map<string, Folder[]>();
  const roots: Folder[] = [];
  for (const f of folders) {
    if (f.parent_remote_id && ids.has(f.parent_remote_id)) {
      const k = kids.get(f.parent_remote_id) ?? []; k.push(f); kids.set(f.parent_remote_id, k);
    } else roots.push(f);
  }
  const out: { f: Folder; depth: number }[] = [];
  const walk = (list: Folder[], depth: number) => { for (const f of list) { out.push({ f, depth }); walk(kids.get(f.remote_id) ?? [], depth + 1); } };
  walk(roots, 0);
  return out;
}

/** Keeps a crash in one pane from taking the whole app down, and says what happened in plain words. */
class PaneBoundary extends React.Component<{ children: React.ReactNode }, { err: string | null }> {
  state = { err: null as string | null };
  static getDerivedStateFromError(e: Error) { return { err: e.message }; }
  componentDidCatch(e: Error) { invoke("log_js", { message: `calendar crashed: ${e.message}\n${e.stack}` }); }
  render() {
    return this.state.err
      ? <section className="calendar"><div className="empty">The calendar couldn't be shown ({this.state.err}).<br /><button onClick={() => this.setState({ err: null })}>Try again</button></div></section>
      : this.props.children;
  }
}

export default function Mailbox({ account: _first, onSignOut }: { account: Account; onSignOut: () => void }) {
  const [folders, setFolders] = useState<Folder[]>([]);
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [collapsed, setCollapsed] = useState<Set<string>>(() => { try { return new Set(JSON.parse(localStorage.getItem("collapsedAccts") || "[]")); } catch { return new Set(); } });
  useEffect(() => { try { localStorage.setItem("collapsedAccts", JSON.stringify([...collapsed])); } catch { /* ignore */ } }, [collapsed]);
  const colorOf = (email: string) => PALETTE[Math.max(0, accounts.findIndex((a) => a.email.toLowerCase() === email.toLowerCase())) % PALETTE.length];
  const multi = accounts.length > 1;
  const [folderId, setFolderId] = useState<string | null>(null);
  const [msgs, setMsgs] = useState<Msg[]>([]);
  const [selected, setSelected] = useState<Detail | null>(null);
  const [query, setQuery] = useState("");
  const [syncing, setSyncing] = useState(false);
  const [status, setStatus] = useState("");
  const [online, setOnline] = useState(navigator.onLine);
  // Remote images: loaded for this message only ("Load images"), or always for senders the user trusts.
  const [allowRemote, setAllowRemote] = useState(false);
  const [trustedSender, setTrustedSender] = useState(false);
  const showImages = allowRemote || trustedSender;
  const [themeChoice, setThemeChoice] = useState<ThemeChoice>(cachedChoice);
  const [sysDark, setSysDark] = useState(() => matchMedia("(prefers-color-scheme: dark)").matches);
  const dark = isDark(resolveTheme(themeChoice, sysDark));
  const [sel, setSel] = useState<Selection>({ ids: new Set(), anchor: null });
  const [ctx, setCtx] = useState<null | { x: number; y: number }>(null);
  const [picker, setPicker] = useState<null | "move" | "copy">(null);
  const [ruleInit, setRuleInit] = useState<null | { rule: QuickRule; account: string }>(null);
  const [ruleBusy, setRuleBusy] = useState(false);
  const [ruleErr, setRuleErr] = useState("");
  const [darkMail, setDarkMail] = useState(true);
  const scroller = useRef<HTMLDivElement>(null);
  const [compose, setCompose] = useState<ComposeInit | null>(null);
  const [toast, setToast] = useState("");
  const [queue, setQueue] = useState<QueueItem[]>([]);
  const [queueOpen, setQueueOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [atts, setAtts] = useState<Att[]>([]);
  const [attBusy, setAttBusy] = useState("");
  const [rulesOpen, setRulesOpen] = useState(false);
  const [drafts, setDrafts] = useState<DraftRow[]>([]);
  const [draftsOpen, setDraftsOpen] = useState(false);
  const refreshDrafts = () => invoke<DraftRow[]>("list_drafts").then(setDrafts).catch(() => {});
  const splitAddr = (s: string) => s.split(/[,;\s]+/).map((x) => x.trim()).filter(Boolean);
  const openDraft = (d: DraftRow) => {
    setDraftsOpen(false);
    setCompose({ mode: d.mode, replyTo: d.reply_to ?? undefined, from: d.from ?? undefined, to: splitAddr(d.to), cc: splitAddr(d.cc), bcc: splitAddr(d.bcc ?? ""), subject: d.subject, draftId: d.id, html: d.html, attachments: d.attachments });
  };
  const [tab, setTab] = useState<RibbonTab>("home");
  const [meetingReq, setMeetingReq] = useState<{ teams: boolean; n: number; attendees?: string[]; account?: string }>({ teams: true, n: 0 });
  const [prompt, setPrompt] = useState<null | { kind: "new" | "sub" | "rename" }>(null);
  const pending = queue.filter((q) => !q.failed).length;
  const rejected = queue.filter((q) => q.failed).length;
  const refreshQueue = () => invoke<QueueItem[]>("pending_details").then(setQueue).catch(() => {});
  const [, setMoveOpen] = useState(false);
  const [view, setView] = useState<"mail" | "calendar" | "contacts">("mail");
  const [cals, setCals] = useState<Cal[]>([]);
  const [hiddenCals, setHiddenCals] = useState<Set<string>>(() => { try { return new Set(JSON.parse(localStorage.getItem("hiddenCals") || "[]")); } catch { return new Set(); } });
  useEffect(() => { try { localStorage.setItem("hiddenCals", JSON.stringify([...hiddenCals])); } catch { /* ignore */ } }, [hiddenCals]);
  const loadCals = () => invoke<Cal[]>("list_calendars").then(setCals).catch(() => {});
  const viewRef = useRef({ folderId: null as string | null, query: "" });
  viewRef.current = { folderId, query };

  useEffect(() => { applyTheme(themeChoice); }, [themeChoice, sysDark]);
  useEffect(() => {
    invoke<string>("get_theme").then((t) => isChoice(t) && setThemeChoice(t)).catch(() => {});
    const mq = matchMedia("(prefers-color-scheme: dark)");
    const on = () => setSysDark(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  const chooseTheme = (t: ThemeChoice) => { setThemeChoice(t); invoke("set_theme", { theme: t }).catch((e) => flash(String(e))); };

  const loadFolders = async () => {
    loadCals();
    const [f, accs] = await Promise.all([invoke<Folder[]>("list_folders"), invoke<Account[]>("list_accounts")]);
    setFolders(f);
    setAccounts(accs);
    const firstInbox = f.find((x) => x.well_known === "inbox")?.id ?? f[0]?.id ?? null;
    setFolderId((cur) => {
      if (cur === ALL) return accs.length > 1 ? ALL : firstInbox;
      if (cur && f.some((x) => x.id === cur)) return cur;
      return accs.length > 1 ? ALL : firstInbox;
    });
    return accs;
  };

  const loadMessages = async () => {
    const { folderId, query } = viewRef.current;
    if (query.trim()) setMsgs(await invoke<Msg[]>("search_messages", { query }));
    else if (folderId) setMsgs(await invoke<Msg[]>("list_messages", { folderId, offset: 0, limit: PAGE }));
    refreshQueue();
  };

  const flash = (t: string) => { setToast(t); setTimeout(() => setToast(""), 2600); };
  const patch = (id: string, p: Partial<Msg>) => {
    setMsgs((ms) => ms.map((m) => (m.id === id ? { ...m, ...p } : m)));
    setSelected((s) => (s && s.id === id ? { ...s, ...p } : s));
  };
  /** Takes messages out of the list; when the open one goes, the next one opens. */
  const removeMany = (ids: string[]) => {
    if (!ids.length) return;
    const gone = new Set(ids);
    const idx = msgs.findIndex((m) => gone.has(m.id));
    const rest = msgs.filter((m) => !gone.has(m.id));
    setMsgs(rest);
    if (selected && !gone.has(selected.id)) return;
    const next = rest[idx] ?? rest[idx - 1];
    if (next) open(next); else { setSelected(null); setSel({ ids: new Set(), anchor: null }); }
  };
  /** Runs one queued action per message (each goes through the offline outbox); returns the ids that worked. */
  const runAll = async (list: Msg[], fn: (m: Msg) => Promise<unknown>) => {
    const done: string[] = [];
    let err = "";
    for (const m of list) {
      try { await fn(m); done.push(m.id); } catch (e) { err = String(e); }
    }
    loadFolders(); refreshQueue();
    if (err) flash(list.length > 1 ? `${list.length - done.length} of ${list.length} couldn't be changed: ${err}` : err);
    return done;
  };
  const n = (k: number) => (k === 1 ? "1 message" : `${k} messages`);
  const markRead = (list: Msg[]) => {
    const read = bulkRead(list);
    return runAll(list.filter((m) => m.is_read !== read), (m) => invoke("set_read", { id: m.id, read }).then(() => patch(m.id, { is_read: read })));
  };
  const markFlag = (list: Msg[]) => {
    const flagged = bulkFlag(list);
    return runAll(list.filter((m) => m.is_flagged !== flagged), (m) => invoke("set_flag", { id: m.id, flagged }).then(() => patch(m.id, { is_flagged: flagged })));
  };
  const del = async (list: Msg[]) => {
    if (!list.length) return;
    const inTrash = folders.find((f) => f.id === folderId)?.well_known === "deleteditems";
    if (inTrash && !(await ask(`Permanently delete ${list.length === 1 ? "this message" : `these ${list.length} messages`}? This can't be undone.`, { title: "Delete", kind: "warning" }))) return;
    removeMany(await runAll(list, (m) => invoke("delete_message", { id: m.id })));
  };
  const moveTo = async (list: Msg[], f: Folder) => {
    setMoveOpen(false);
    const done = await runAll(list, (m) => invoke("move_message", { id: m.id, folderId: f.id }));
    removeMany(done);
    if (done.length) flash(`Moved ${list.length > 1 ? `${n(done.length)} ` : ""}to ${f.name}`);
  };
  const copyTo = async (list: Msg[], f: Folder) => {
    const done = await runAll(list, (m) => invoke("copy_message", { id: m.id, folderId: f.id }));
    if (done.length) flash(`Copied ${list.length > 1 ? `${n(done.length)} ` : ""}to ${f.name}`);
  };
  const markSpam = async (list: Msg[]) => {
    const done = await runAll(list, (m) => invoke("mark_spam", { id: m.id }));
    removeMany(done);
    if (done.length) flash(`${n(done.length)} moved to Junk`);
  };
  /** Rules and blocking need an extra permission (mailbox settings / Gmail filters): ask for it once, then retry. */
  const withRulesPermission = async <T,>(email: string, fn: () => Promise<T>): Promise<T> => {
    try { return await fn(); } catch (e) {
      if (!/extra permission/i.test(String(e))) throw e;
      const gmail = providerOf(email) === "google";
      if (!(await ask(`Rules and blocking need one extra permission for ${email} (${gmail ? "managing Gmail filters" : "managing mailbox settings"}). Open the browser to allow it?`, { title: "Permission needed", kind: "info", okLabel: "Allow", cancelLabel: "Cancel" }))) throw new Error("Nothing was changed.");
      await invoke("enable_rules", { email });
      await loadFolders();
      return await fn();
    }
  };
  /** Blocks each sender once (future mail goes to Junk / Spam) and moves these messages there too. */
  const blockSenders = async (list: Msg[]) => {
    const firstPer = new Map<string, Msg>();
    for (const m of list) if (m.sender_address) firstPer.set(`${m.account_email}\n${m.sender_address.toLowerCase()}`, m);
    if (!firstPer.size) return flash("There is no sender address to block.");
    const who = [...new Set([...firstPer.values()].map((m) => m.sender_address))];
    if (!(await ask(`Block ${who.length === 1 ? who[0] : `${who.length} senders`}? Their future mail goes to Junk, and ${list.length === 1 ? "this message moves" : "these messages move"} there now.`, { title: "Block sender", kind: "warning", okLabel: "Block", cancelLabel: "Cancel" }))) return;
    const blocked: string[] = [];
    for (const m of firstPer.values()) {
      try { blocked.push(await withRulesPermission(m.account_email, () => invoke<string>("block_sender", { id: m.id }))); }
      catch (e) { flash(String(e)); }
    }
    if (!blocked.length) return;
    const hit = new Set(blocked.map((b) => b.toLowerCase()));
    removeMany(await runAll(list.filter((m) => m.sender_address && hit.has(m.sender_address.toLowerCase())), (m) => invoke("mark_spam", { id: m.id })));
    flash(`Blocked ${blocked.length === 1 ? blocked[0] : `${blocked.length} senders`}`);
  };
  const saveQuickRule = async (r: QuickRule) => {
    if (!ruleInit) return;
    const account = ruleInit.account;
    setRuleBusy(true); setRuleErr("");
    const rule = { id: null, account, name: "", enabled: true, from: r.field === "from" ? r.text : "", subject: r.field === "subject" ? r.text : "", to: "", body: "",
      move_to: r.action === "move" ? r.folder : null, mark_read: r.action === "read", flag: false, delete: r.action === "delete" };
    try {
      await withRulesPermission(account, () => invoke("save_rule", { rule }));
      setRuleInit(null);
      flash("Rule created");
    } catch (e) { setRuleErr(String(e)); } finally { setRuleBusy(false); }
  };

  const addrs = (json: string | null) => { try { return (JSON.parse(json || "[]") as { address?: string }[]).map((p) => p.address!).filter(Boolean); } catch { return []; } };
  /** The account a brand-new message should come from: the folder you're in, else the open message's account. */
  const newMail = () => setCompose({ mode: "new", from: folders.find((f) => f.id === folderId)?.account_email ?? selected?.account_email ?? accounts[0]?.email });
  const startCompose = (mode: ComposeInit["mode"]) => {
    if (mode === "new" || !selected) return newMail();
    const s = selected;
    const me = s.account_email.toLowerCase();
    const header = `On ${new Date(s.received_at * 1000).toLocaleString()}, ${s.sender_name ?? ""} <${s.sender_address ?? ""}> wrote:`; // plain text
    const subj = s.subject ?? "";
    const base = { replyTo: s.id, from: s.account_email, quoteHtml: s.body_html ?? "", quoteHeader: header, quoteImages: showImages };
    if (mode === "forward") return setCompose({ ...base, mode, subject: /^fw(d)?:/i.test(subj) ? subj : `Fw: ${subj}` });
    const re = /^re:/i.test(subj) ? subj : `Re: ${subj}`;
    const to = [s.sender_address ?? ""].filter(Boolean);
    if (mode === "reply") return setCompose({ ...base, mode, to, subject: re });
    const others = [...addrs(s.to_json), ...addrs(s.cc_json)].filter((a) => a.toLowerCase() !== me && !to.includes(a));
    setCompose({ ...base, mode, to, cc: others, subject: re });
  };

  // Keyboard shortcuts (ignored while typing)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (compose || t.tagName === "INPUT" || t.isContentEditable) return;
      if (ctx || picker || ruleInit || settingsOpen || rulesOpen || prompt || t.tagName === "SELECT" || t.tagName === "TEXTAREA") return;
      const idx = selected ? msgs.findIndex((m) => m.id === selected.id) : -1;
      if (view === "mail" && (e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a" && msgs.length) { e.preventDefault(); setSel(selectAll(msgs.map((m) => m.id))); }
      else if (e.key === "Escape" && sel.ids.size > 1) setSel(selected ? single(selected.id) : { ids: new Set(), anchor: null });
      else if ((e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) && targets.length) {
        e.preventDefault();
        const r = document.querySelector(".msg.active")?.getBoundingClientRect();
        setCtx({ x: r ? r.left + 40 : 300, y: r ? r.top + r.height / 2 : 200 });
      }
      else if (e.key === "ArrowDown" && msgs[idx + 1]) { e.preventDefault(); open(msgs[idx + 1]); }
      else if (e.key === "ArrowUp" && idx > 0) { e.preventDefault(); open(msgs[idx - 1]); }
      else if (e.key === "Delete" && targets.length) del(targets);
      else if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "u" && targets.length) markRead(targets);
      else if (!selected || multiSel) { if ((e.ctrlKey || e.metaKey) && e.key === "n") newMail(); }
      else if ((e.ctrlKey || e.metaKey) && e.key === "n") newMail();
      else if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "r") startCompose("replyAll");
      else if ((e.ctrlKey || e.metaKey) && e.key === "r") { e.preventDefault(); startCompose("reply"); }
      else if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "f") startCompose("forward");
    };
    addEventListener("keydown", onKey);
    return () => removeEventListener("keydown", onKey);
  });

  const syncNow = async () => {
    setSyncing(true);
    setStatus("Syncing…");
    try {
      const r = await invoke<SyncReport>("sync_now");
      setStatus(syncStatus(r));
      await loadFolders();
      await loadMessages();
    } catch (e) {
      setStatus(navigator.onLine ? `Sync failed: ${e}` : "Offline – showing saved mail");
    } finally {
      setSyncing(false);
    }
  };

  useEffect(() => {
    refreshDrafts();
    loadFolders().then(() => syncNow());
    const un = listen<{ Ok?: SyncReport; Err?: string }>("sync-finished", (e) => {
      if (e.payload.Ok) setStatus(syncStatus(e.payload.Ok));
      else if (e.payload.Err) setStatus(`Sync failed: ${e.payload.Err}`);
      loadFolders(); loadMessages();
    });
    // Links clicked inside an email are handled by the app shell (Rust) and reported here.
    const un2 = listen<string>("mailto", (e) => setCompose({ mode: "new", to: e.payload ? [e.payload] : [] }));
    const un3 = listen<string>("link-failed", () => flash("Couldn't open the link in your web browser."));
    const on = () => setOnline(true), off = () => setOnline(false);
    addEventListener("online", on);
    addEventListener("offline", off);
    return () => { un.then((f) => f()); un2.then((f) => f()); un3.then((f) => f()); removeEventListener("online", on); removeEventListener("offline", off); };
  }, []);

  useEffect(() => {
    setSel({ ids: new Set(), anchor: null });
    const t = setTimeout(loadMessages, query ? 150 : 0);
    return () => clearTimeout(t);
  }, [folderId, query]);

  useEffect(() => { setSel((s) => prune(msgs.map((m) => m.id), s)); }, [msgs]);

  const loadMore = async () => {
    if (query.trim() || !folderId || msgs.length % PAGE !== 0 || msgs.length === 0) return;
    const more = await invoke<Msg[]>("list_messages", { folderId, offset: msgs.length, limit: PAGE });
    if (more.length) setMsgs((m) => [...m, ...more]);
  };

  const virt = useVirtualizer({ count: msgs.length, getScrollElement: () => scroller.current, estimateSize: () => 76, overscan: 10 });
  const items = virt.getVirtualItems();
  useEffect(() => {
    const last = items[items.length - 1];
    if (last && last.index >= msgs.length - 20) loadMore();
  }, [items]);

  const open = async (m: Msg) => {
    setAllowRemote(false);
    setTrustedSender(false);
    setMoveOpen(false);
    setSel(single(m.id));
    const d = await invoke<Detail>("get_message", { id: m.id });
    setSelected(d);
    setAtts([]);
    if (m.sender_address) invoke<boolean>("image_sender_allowed", { address: m.sender_address }).then((ok) => setSelected((s) => { if (s && s.id === m.id) setTrustedSender(!!ok); return s; })).catch(() => {});
    if (m.has_attachments) invoke<Att[]>("list_attachments", { messageId: m.id }).then((a) => setSelected((s) => { if (s && s.id === m.id) setAtts(a); return s; })).catch(() => {});
    if (!m.is_read) {
      invoke("set_read", { id: m.id, read: true }).then(() => { patch(m.id, { is_read: true }); loadFolders(); }).catch(() => {});
    }
  };

  const rendered = useMemo(
    () => (selected?.body_html ? buildDoc(selected.body_html, showImages, dark && darkMail) : null),
    [selected, showImages, dark, darkMail],
  );
  // How many remote images the message has (also when they are shown, for the "always load" bar).
  const remoteImages = useMemo(
    () => (!selected?.body_html ? 0 : showImages ? buildDoc(selected.body_html, false, false).blocked : rendered?.blocked ?? 0),
    [selected, showImages, rendered],
  );
  const trustSender = async (allowed: boolean) => {
    const address = selected?.sender_address;
    if (!address) return;
    try {
      await invoke("set_image_sender", { address, allowed });
      setTrustedSender(allowed);
      if (!allowed) setAllowRemote(false);
      flash(allowed ? `Images from ${address} will always be loaded` : `Images from ${address} will be blocked again`);
    } catch (e) { flash(String(e)); }
  };

  const current = folders.find((f) => f.id === folderId);
  const inboxes = folders.filter((f) => f.well_known === "inbox");
  const allUnread = inboxes.reduce((n, f) => n + f.unread, 0);
  const allTotal = inboxes.reduce((n, f) => n + f.total, 0);
  // Which tile each folder gets (by role, never by name).
  const roles = useMemo(() => folderRoles(folders), [folders]);
  const groups = useMemo(
    () => accounts.map((acc) => ({ acc, tree: buildTree(folders.filter((f) => f.account_email.toLowerCase() === acc.email.toLowerCase())) })),
    [accounts, folders],
  );
  const providerOf = (email?: string) => accounts.find((a) => a.email.toLowerCase() === email?.toLowerCase())?.provider ?? "microsoft";

  const builtIn = !!current?.well_known;
  const folderAction = async (name: string, forAccount?: string) => {
    const kind = prompt?.kind;
    setPrompt(null);
    try {
      if (kind === "new") { const id = await invoke<string>("create_folder", { name, parentId: null, account: forAccount ?? null }); flash(`Folder "${name}" created`); await loadFolders(); setFolderId(id); }
      else if (kind === "sub" && current) { const id = await invoke<string>("create_folder", { name, parentId: current.id }); flash(`Folder "${name}" created inside ${current.name}`); await loadFolders(); setFolderId(id); }
      else if (kind === "rename" && current) { await invoke("rename_folder", { id: current.id, name }); flash("Folder renamed"); loadFolders(); }
    } catch (e) { flash(String(e)); }
  };
  const deleteFolder = async () => {
    if (!current || builtIn) return;
    const gmail = isGmail(providerOf(current.account_email));
    if (!(await ask(gmail ? `Delete the label "${current.name}"? Its messages stay in your Gmail account (in All Mail).` : `Delete the folder "${current.name}" and everything in it? Microsoft 365 moves it to Deleted Items.`, { title: "Delete folder", kind: "warning" }))) return;
    try {
      await invoke("delete_folder", { id: current.id });
      flash("Folder deleted");
      setFolderId(folders.find((f) => f.well_known === "inbox" && f.account_email === current.account_email)?.id ?? null);
      setSelected(null);
      loadFolders();
    } catch (e) { flash(String(e)); }
  };
  const archiveFolder = folders.find((f) => f.well_known === "archive" && f.account_email === selected?.account_email);
  // What the ribbon, keyboard and right-click act on: the selected messages, else the open one.
  const listed = msgs.filter((m) => sel.ids.has(m.id));
  const targets: Msg[] = listed.length ? listed : selected ? [selected] : [];
  const multiSel = targets.length > 1;
  const one = targets.length === 1 && selected?.id === targets[0].id;
  // Moving and copying stay within one account.
  const tAcct = targets.length && targets.every((m) => m.account_email === targets[0].account_email) ? targets[0].account_email : null;
  const folderChoices = (kind: "move" | "copy") => (tAcct ? buildTree(folders.filter((f) => f.account_email === tAcct)) : [])
    .filter(({ f }) => f.id !== folderId && !(isGmail(providerOf(f.account_email)) && (f.well_known === "sentitems" || f.well_known === "drafts"))
      && !(kind === "copy" && ["deleteditems", "junkemail", "sentitems", "drafts"].includes(f.well_known ?? "")));
  const inJunk = current?.well_known === "junkemail";
  const markReadLabel = bulkRead(targets) ? "Mark Read" : "Mark Unread";
  const flagLabel = bulkFlag(targets) ? "Flag" : "Clear Flag";
  const openRuleDialog = () => {
    const m = targets[0];
    setRuleErr("");
    setRuleInit({ account: m.account_email, rule: { field: "from", text: m.sender_address ?? "", action: "move", folder: "" } });
  };
  const ctxItems: MenuItem[] = [
    { label: "Reply", icon: "reply", onClick: () => startCompose("reply"), disabled: !one },
    { label: "Reply All", icon: "replyall", onClick: () => startCompose("replyAll"), disabled: !one },
    { label: "Forward", icon: "forward", onClick: () => startCompose("forward"), disabled: !one },
    { label: multiSel ? `Delete ${targets.length}` : "Delete", icon: "trash", onClick: () => del(targets), separator: true, destructive: true },
    { label: "Move to…", icon: "move", onClick: () => setPicker("move"), disabled: !tAcct },
    { label: "Copy to…", icon: "copy", onClick: () => setPicker("copy"), disabled: !tAcct },
    { label: markReadLabel, icon: bulkRead(targets) ? "read" : "unread", onClick: () => markRead(targets), separator: true },
    { label: flagLabel, icon: "flag", onClick: () => markFlag(targets) },
    { label: "Create rule…", icon: "rules", onClick: openRuleDialog, disabled: targets.length !== 1, separator: true },
    { label: "Block sender", icon: "block", onClick: () => blockSenders(targets), disabled: !targets.some((m) => m.sender_address), destructive: true },
    { label: "Mark as spam", icon: "junkemail", onClick: () => markSpam(targets), disabled: inJunk, destructive: true },
  ];
  const newMeeting = (teams: boolean, attendees?: string[], account?: string) => { setView("calendar"); setMeetingReq((r) => ({ teams, n: r.n + 1, attendees, account })); };
  /** Contacts → Block sender: the same server rule as blocking from a message (future mail goes to Junk). */
  const blockAddress = async (account: string, address: string) => {
    if (!(await ask(`Block ${address}? Their future mail to ${account} goes to Junk.`, { title: "Block sender", kind: "warning", okLabel: "Block", cancelLabel: "Cancel" }))) return;
    try { flash(`Blocked ${await withRulesPermission(account, () => invoke<string>("block_address", { account, address }))}`); }
    catch (e) { flash(String(e)); }
  };
  const ruleForAddress = (account: string, address: string) => {
    setRuleErr("");
    setRuleInit({ account, rule: { field: "from", text: address, action: "move", folder: "" } });
  };
  const searchMailWith = (addresses: string[]) => {
    setView("mail"); setSelected(null); setFolderId(ALL); setQuery(`with:${addresses.join(",")}`);
  };
  // A message can only be moved within its own account.
  const moveItems = folderChoices("move").map(({ f, depth }) => ({ label: f.name, indent: depth, icon: <FolderTile role={roles.get(f.id) ?? "folder"} size={20} glyph={13} radius={6} />, onClick: () => moveTo(targets, f) }));
  const mailTab = view === "mail";
  const initials = (n?: string | null) => (n || "?").split(/\s+/).map((w) => w[0]).slice(0, 2).join("").toUpperCase();

  return (
    <div className="shell">
      <nav className="sidebar">
        <DragBar><img src="/logo.png" alt="" className="brand-logo" /><span className="brand-name">Graphite Mail</span></DragBar>
        <div className="navcards">
          <div role="button" tabIndex={0} aria-pressed={view === "calendar"} className={`navcard ${view === "calendar" ? "active" : ""}`}
            onClick={() => setView("calendar")} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setView("calendar"); } }}>
            <FolderTile role="calendar" size={38} glyph={22} radius={10} />
            <div className="navcard-text">
              <span className="name" title="All your accounts' calendars together">Master Calendar</span>
              <span className="sub">All calendars</span>
            </div>
          </div>
          <div role="button" tabIndex={0} aria-pressed={view === "contacts"} className={`navcard ${view === "contacts" ? "active" : ""}`}
            onClick={() => setView("contacts")} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setView("contacts"); } }}>
            <FolderTile role="contacts" size={38} glyph={22} radius={10} />
            <div className="navcard-text">
              <span className="name" title="Your organisation's directory, your contacts, and people you've emailed">Contacts</span>
              <span className="sub">People and directory</span>
            </div>
          </div>
        </div>
        <div className="folders">
          {view === "calendar" && cals.length > 0 && <div className="section" style={{ padding: "6px 10px 4px" }}>My calendars <span style={{ fontWeight: 400 }}>· tick to show</span></div>}
          {view === "calendar" && cals.map((c, i) => (
            <React.Fragment key={c.id}>
            {multi && (i === 0 || cals[i - 1].account_email !== c.account_email) && <div className="muted acct-sub">{accounts.find((a) => a.email.toLowerCase() === c.account_email.toLowerCase())?.display_name ?? c.account_email}</div>}
            <div className="folder calrow" onClick={() => setView("calendar")} title="Open calendar">
              <input type="checkbox" checked={!hiddenCals.has(c.id)} style={{ accentColor: c.color }} title="Show or hide this calendar"
                onClick={(e) => e.stopPropagation()}
                onChange={() => setHiddenCals((h) => { const n = new Set(h); n.has(c.id) ? n.delete(c.id) : n.add(c.id); return n; })} />
              <span className="name">{c.name}</span>
            </div>
            </React.Fragment>
          ))}
          <div className="section" style={{ padding: "14px 10px 4px" }}>Mailboxes</div>
          {multi && (
            <div className={`folder allinboxes ${folderId === ALL && !query && view === "mail" ? "active" : ""}`} onClick={() => { setView("mail"); setQuery(""); setFolderId(ALL); setSelected(null); }}>
              <FolderTile role="allinboxes" size={26} glyph={16} radius={7} />
              <span className="name">All Inboxes</span>
              {allUnread > 0 && <span className="count">{allUnread}</span>}
            </div>
          )}
          {groups.map(({ acc, tree }) => {
            const shut = collapsed.has(acc.email);
            return (
              <div key={acc.email} className="acct">
                <div className="acct-head" onClick={() => setCollapsed((c) => { const n = new Set(c); n.has(acc.email) ? n.delete(acc.email) : n.add(acc.email); return n; })} title={shut ? "Show folders" : "Hide folders"}>
                  <span className="chev">{shut ? "▸" : "▾"}</span>
                  <i className="acctdot" style={{ background: colorOf(acc.email) }} />
                  <span className="acct-name"><b>{acc.display_name}</b><span className="muted">{acc.email}</span></span>
                </div>
                {!shut && tree.map(({ f, depth }) => (
                  <div key={f.id} style={{ paddingLeft: 10 + depth * 22 }} className={`folder ${f.id === folderId && !query && view === "mail" ? "active" : ""}`} onClick={() => { setView("mail"); setQuery(""); setFolderId(f.id); setSelected(null); }}>
                    <FolderTile role={roles.get(f.id) ?? "folder"} />
                    <span className="name">{f.name}</span>
                    {f.unread > 0 && <span className="count">{f.unread}</span>}
                  </div>
                ))}
              </div>
            );
          })}
          {!folders.length && <div className="empty">No folders yet</div>}
        </div>
        <div className="account">
          <div className="avatar">{multi ? String(accounts.length) : initials(accounts[0]?.display_name ?? _first.display_name)}</div>
          <div className="who">
            <div>{multi ? `${accounts.length} accounts` : accounts[0]?.display_name ?? _first.display_name}</div>
            <div className="muted" title={status}><span className={`dot ${online ? "" : "off"}`} />{online ? status || "Online" : "Offline – saved mail"}</div>
            {queue.length > 0 && (
              <div className="muted queue-link" onClick={() => { refreshQueue(); setQueueOpen(true); }} title="See what is waiting">
                {pending > 0 ? `${pending} waiting to sync` : ""}{pending > 0 && rejected > 0 ? " · " : ""}{rejected > 0 ? `${rejected} not accepted` : ""}
              </div>
            )}
          </div>
          <button className="icon" onClick={() => setSettingsOpen(true)} title="Settings"><Icon name="settings" /></button>
          <button className="icon" onClick={async () => { if (await ask(multi ? "Sign out of all accounts? Their downloaded mail is removed from this computer (nothing is deleted on the servers)." : "Sign out? Your downloaded mail is removed from this computer (nothing is deleted on the server).", { title: "Sign out", kind: "warning" })) onSignOut(); }} title={multi ? "Sign out of all accounts" : "Sign out"}><Icon name="signout" /></button>
        </div>
      </nav>

      <Ribbon tabs={[{ id: "home", label: "Home" }, { id: "folder", label: "Folder" }, { id: "view", label: "View" }]} active={tab} onTab={setTab}>
        {tab === "home" && <>
          <Group label="New">
            <Big icon="compose" label="New Email" accent onClick={() => newMail()} title="New email (Ctrl+N)" />
            <Big icon="meeting" label="New Meeting" onClick={() => newMeeting(true)} title="New Teams meeting" />
            <Stack><Small icon="calendar" label="New Event" onClick={() => newMeeting(false)} /></Stack>
          </Group>
          <Group label="Delete">
            <Big icon="trash" label={multiSel ? `Delete (${targets.length})` : "Delete"} disabled={!mailTab || !targets.length} onClick={() => del(targets)} title="Delete (Del)" />
            <Big icon="archive" label="Archive" disabled={!mailTab || multiSel || !selected || !archiveFolder || folderId === archiveFolder.id} onClick={() => selected && archiveFolder && moveTo([selected], archiveFolder)} />
          </Group>
          <Group label="Respond">
            <Big icon="reply" label="Reply" disabled={!mailTab || !one} onClick={() => startCompose("reply")} title="Reply (Ctrl+R)" />
            <Big icon="replyall" label="Reply All" disabled={!mailTab || !one} onClick={() => startCompose("replyAll")} title="Reply all (Ctrl+Shift+R)" />
            <Big icon="forward" label="Forward" disabled={!mailTab || !one} onClick={() => startCompose("forward")} title="Forward (Ctrl+Shift+F)" />
          </Group>
          <Group label="Move">
            <BigMenu icon="move" label={multiSel ? `Move (${targets.length})` : "Move"} disabled={!mailTab || !tAcct} items={moveItems} />
          </Group>
          <Group label="Tags">
            <Stack>
              <Small icon={bulkRead(targets) ? "read" : "unread"} label={markReadLabel} disabled={!mailTab || !targets.length} onClick={() => markRead(targets)} />
              <Small icon="flag" label={flagLabel} on={!!targets.length && !bulkFlag(targets)} disabled={!mailTab || !targets.length} onClick={() => markFlag(targets)} />
            </Stack>
          </Group>
          <Group label="Drafts">
            <Big icon="drafts" label={drafts.length ? `Drafts (${drafts.length})` : "Drafts"} onClick={() => { refreshDrafts(); setDraftsOpen(true); }} title="Messages you started but haven't sent" />
          </Group>
          <Group label="Rules">
            <Big icon="rules" label="Rules" onClick={() => setRulesOpen(true)} title="Automatically move, mark or delete incoming mail" />
          </Group>
          <Group label="Sync">
            <Big icon="sync" label={syncing ? "Syncing…" : "Send / Receive"} disabled={syncing || !online} onClick={syncNow} />
          </Group>
        </>}
        {tab === "folder" && <>
          <Group label="New">
            <Big icon="newfolder" label="New Folder" onClick={() => setPrompt({ kind: "new" })} />
            <Big icon="subfolder" label="New Subfolder" disabled={!current || view !== "mail"} onClick={() => setPrompt({ kind: "sub" })} title={current ? `New folder inside ${current.name}` : undefined} />
          </Group>
          <Group label={current ? `“${current.name}”` : "Folder"}>
            <Big icon="rename" label="Rename" disabled={!current || builtIn || view !== "mail"} onClick={() => setPrompt({ kind: "rename" })} title={builtIn ? "Built-in folders can't be renamed" : undefined} />
            <Big icon="deletefolder" label="Delete Folder" disabled={!current || builtIn || view !== "mail"} onClick={deleteFolder} title={builtIn ? "Built-in folders can't be deleted" : undefined} />
          </Group>
          <Group label="Sync">
            <Big icon="sync" label="Refresh Folders" disabled={syncing || !online} onClick={syncNow} />
          </Group>
        </>}
        {tab === "view" && <>
          <Group label="Show">
            <Big icon="mail" label="Mail" onClick={() => setView("mail")} accent={view === "mail"} />
            <Big icon="calendar" label="Calendar" onClick={() => setView("calendar")} accent={view === "calendar"} />
            <Big icon="contacts" label="Contacts" onClick={() => setView("contacts")} accent={view === "contacts"} />
          </Group>
          <Group label="Appearance">
            <BigMenu icon="appearance" label="Theme" items={[{ id: "system" as ThemeChoice, label: "Match system" }, ...THEMES].map((t) => ({
              label: `${themeChoice === t.id ? "✓ " : ""}${t.label}`, onClick: () => chooseTheme(t.id),
            }))} />
            <Stack>
              <Small icon={darkMail ? "moon" : "sun"} label={darkMail ? "Emails: dark" : "Emails: original"} disabled={!dark} onClick={() => setDarkMail(!darkMail)} title="How email content looks in dark mode" />
            </Stack>
          </Group>
          <Group label="App">
            <Big icon="settings" label="Settings" onClick={() => setSettingsOpen(true)} />
          </Group>
        </>}
      </Ribbon>

      {view === "calendar" && <PaneBoundary><CalendarView calendars={cals} hidden={hiddenCals} onToast={flash} newRequest={meetingReq} /></PaneBoundary>}
      {view === "contacts" && <PaneBoundary><ContactsView accounts={accounts} onToast={flash} onNewEmail={(to, from) => setCompose({ mode: "new", to, from })}
        onNewMeeting={(attendees, account) => newMeeting(true, attendees, account)} onSearchMail={searchMailWith} onBlock={blockAddress} onCreateRule={ruleForAddress} /></PaneBoundary>}
      {view === "mail" && <>
      <section className="list">
        <div className="toolbar" data-tauri-drag-region>
          <div className="title" data-tauri-drag-region>
            <b>{query.startsWith("with:") ? `Mail with ${query.slice(5).split(",").join(", ")}` : query ? "Search" : folderId === ALL ? "All Inboxes" : current?.name ?? "Mail"}</b>
            <span>{query ? `${msgs.length} results` : folderId === ALL ? `${allTotal} messages${allUnread ? `, ${allUnread} unread` : ""}` : current ? `${current.total} messages${current.unread ? `, ${current.unread} unread` : ""}${multi ? ` · ${current.account_email}` : ""}` : ""}</span>
          </div>
          <button className="icon" onClick={syncNow} disabled={syncing || !online} title="Get new mail"><span className={syncing ? "spin" : ""}><Icon name="refresh" /></span></button>
        </div>
        <div className="search">
          <input placeholder="Search" value={query} onChange={(e) => setQuery(e.target.value)} />
        </div>
        <div className={`scroller ${multiSel ? "picking" : ""}`} ref={scroller} aria-multiselectable="true">
          {msgs.length === 0 ? (
            <div className="empty">{query ? "No matches" : syncing ? "Downloading mail…" : "No messages"}</div>
          ) : (
            <div style={{ height: virt.getTotalSize(), position: "relative" }}>
              {items.map((v) => {
                const m = msgs[v.index];
                return (
                  <div key={m.id} className={`msg ${m.is_read ? "" : "unread"} ${sel.ids.has(m.id) ? "active" : ""}`} aria-selected={sel.ids.has(m.id)}
                    style={{ top: v.start }}
                    onClick={(e) => {
                      if (e.ctrlKey || e.metaKey || e.shiftKey) setSel((s) => clickSelect(msgs.map((x) => x.id), s, m.id, { ctrl: e.ctrlKey || e.metaKey, shift: e.shiftKey }));
                      else open(m);
                    }}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      if (!sel.ids.has(m.id)) open(m);
                      setCtx({ x: e.clientX, y: e.clientY });
                    }}>
                    <input type="checkbox" className="msgcheck" checked={sel.ids.has(m.id)} aria-label="Select message" tabIndex={-1}
                      onClick={(e) => e.stopPropagation()}
                      onChange={() => setSel((s) => clickSelect(msgs.map((x) => x.id), s, m.id, { ctrl: true }))} />
                    <div className="top">
                      <span className="from">{multi && (folderId === ALL || query) && <i className="acctdot" style={{ background: colorOf(m.account_email) }} title={m.account_email} />}{m.sender_name || m.sender_address || "(no sender)"}</span>
                      <span className="date">{m.has_attachments && <Icon name="paperclip" size={12} />}{m.is_flagged && <span style={{ color: "#ff9f0a" }}><Icon name="flag" size={12} /></span>}{fmtDate(m.received_at)}</span>
                    </div>
                    <div className="subj">{m.subject || "(no subject)"}</div>
                    <div className="snip">{m.snippet}</div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </section>

      <section className="reader">
        {multiSel ? (
          <div className="multisel">
            <b>{targets.length} messages selected</b>
            <div className="rowbtns" style={{ justifyContent: "center" }}>
              <button onClick={() => markRead(targets)}>{markReadLabel}</button>
              <button onClick={() => markFlag(targets)}>{flagLabel}</button>
              <button onClick={() => del(targets)}>Delete</button>
            </div>
            <span style={{ fontSize: 12 }}>Ctrl+click or Shift+click to change the selection · Esc to clear</span>
          </div>
        ) : selected ? (
          <>
            <header>
              <h2>{selected.subject || "(no subject)"}</h2>
              <div className="sender">
                <div className="avatar">{initials(selected.sender_name || selected.sender_address)}</div>
                <div className="meta">
                  <div><b>{selected.sender_name || selected.sender_address}</b> &lt;{selected.sender_address}&gt;</div>
                  {people(selected.to_json) && <div>To: {people(selected.to_json)}</div>}
                  {people(selected.cc_json) && <div>Cc: {people(selected.cc_json)}</div>}
                  {multi && <div><i className="acctdot" style={{ background: colorOf(selected.account_email) }} />Received in {selected.account_email}</div>}
                </div>
                <div className="when">{new Date(selected.received_at * 1000).toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}</div>
              </div>
            </header>
            {atts.length > 0 && (
              <div className="attrow">
                {atts.map((a) => (
                  <span key={a.id} className="attchip" title={`${a.name} · ${fmtBytes(a.size)}${a.downloaded ? " · saved for offline" : ""}`}>
                    <button className="attopen" disabled={attBusy === a.id}
                      onClick={async () => { setAttBusy(a.id); try { await invoke("open_attachment", { id: a.id }); setAtts((l) => l.map((x) => (x.id === a.id ? { ...x, downloaded: true } : x))); } catch (e) { flash(String(e)); } finally { setAttBusy(""); } }}>
                      <Icon name="paperclip" size={13} /> <span className="attname">{a.name}</span> <span className="muted">{attBusy === a.id ? "downloading…" : fmtBytes(a.size)}</span>
                    </button>
                    <button className="attsave" title="Save a copy…" disabled={attBusy === a.id}
                      onClick={async () => { const dest = await saveDialog({ defaultPath: a.name, title: "Save attachment" }); if (!dest) return; setAttBusy(a.id); try { const at = await invoke<string>("save_attachment", { id: a.id, dest }); flash(at ? `Saved as ${at}` : "Saved"); } catch (e) { flash(String(e)); } finally { setAttBusy(""); } }}>Save</button>
                  </span>
                ))}
              </div>
            )}
            {rendered && remoteImages > 0 && !showImages && (
              <div className="notice imgbar">
                <span>Images from the internet were blocked to protect your privacy.</span>
                <span className="imgbar-actions">
                  <button onClick={() => setAllowRemote(true)}>Load images</button>
                  {selected.sender_address && <button onClick={() => trustSender(true)}>Always load from {selected.sender_address}</button>}
                </span>
              </div>
            )}
            {rendered && remoteImages > 0 && trustedSender && (
              <div className="notice imgbar">
                <span>Images from {selected.sender_address} are always loaded.</span>
                <button onClick={() => trustSender(false)}>Stop loading them</button>
              </div>
            )}
            {rendered ? (
              <div className="bodywrap">
                <MessageFrame doc={rendered.doc} title="message" />
              </div>
            ) : (
              <div className="empty">This message hasn't been downloaded for offline reading yet. It will appear after the next sync.</div>
            )}
          </>
        ) : (
          <div className="empty">No message selected</div>
        )}
      </section>
      </>}
      {compose && (
        <Compose init={compose} accounts={accounts} onClose={() => { setCompose(null); refreshDrafts(); }}
          onSent={(queued) => { setCompose(null); refreshDrafts(); flash(queued ? "You're offline – message will send when you reconnect" : "Message sent"); refreshQueue(); }} />
      )}
      {draftsOpen && (
        <div className="sheet-backdrop" onClick={() => setDraftsOpen(false)}>
          <div className="sheet evsheet" onClick={(e) => e.stopPropagation()}>
            <div className="sheet-bar"><span /><b>Drafts</b><button onClick={() => setDraftsOpen(false)}>Close</button></div>
            <div className="evbody">
              {drafts.length === 0 && <div className="muted">No drafts. When you cancel a message you can save it here to finish later; unsent messages are also saved automatically while you write.</div>}
              {drafts.map((d) => (
                <div key={d.id} className="rulecard">
                  <div className="rulehead"><b>{d.subject || "(no subject)"}</b><span className="muted" style={{ fontSize: 12 }}>{new Date(d.updated_at * 1000).toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}</span></div>
                  <div className="muted" style={{ fontSize: 12.5 }}>{d.to ? `To: ${d.to}` : "No recipient yet"}{d.from && multi ? ` · from ${d.from}` : ""}</div>
                  <div className="rowbtns" style={{ marginTop: 6 }}>
                    <button className="primary" onClick={() => openDraft(d)}>Open</button>
                    <button onClick={async () => { if (await ask("Delete this draft?", { title: "Delete draft", kind: "warning" })) { await invoke("delete_draft", { id: d.id }).catch((e) => flash(String(e))); refreshDrafts(); } }}>Delete</button>
                  </div>
                </div>
              ))}
              <div className="muted" style={{ fontSize: 11.5 }}>Drafts are kept on this computer only; they don't appear in the Drafts folder on the server.</div>
            </div>
          </div>
        </div>
      )}
      {rulesOpen && <Rules accounts={accounts} folders={folders} onClose={() => setRulesOpen(false)} onToast={flash} onAccountsChanged={() => { loadFolders(); }} />}
      {ctx && targets.length > 0 && <ContextMenu x={ctx.x} y={ctx.y} items={ctxItems} onClose={() => setCtx(null)} />}
      {picker && (
        <FolderPicker title={`${picker === "move" ? "Move" : "Copy"} ${n(targets.length)} to…`}
          folders={folderChoices(picker).map(({ f, depth }) => ({ id: f.id, name: f.name, depth, role: roles.get(f.id) ?? "folder" }))}
          onCancel={() => setPicker(null)}
          onPick={(id) => { const f = folders.find((x) => x.id === id); const k = picker; setPicker(null); if (f) (k === "move" ? moveTo : copyTo)(targets, f); }} />
      )}
      {ruleInit && (
        <RuleDialog init={ruleInit.rule} busy={ruleBusy} error={ruleErr} onCancel={() => setRuleInit(null)} onSave={saveQuickRule}
          folders={buildTree(folders.filter((f) => f.account_email === ruleInit.account)).filter(({ f }) => !["sentitems", "drafts"].includes(f.well_known ?? "")).map(({ f, depth }) => ({ id: f.id, name: f.name, depth }))} />
      )}
      {settingsOpen && <Settings accounts={accounts} theme={themeChoice} onTheme={chooseTheme} onClose={() => setSettingsOpen(false)}
        onAccountsChanged={async () => {
          const accs = await loadFolders();
          if (!accs.length) { setSettingsOpen(false); onSignOut(); } else syncNow();
        }} />}
      {prompt && (
        <NamePrompt
          title={prompt.kind === "rename" ? `Rename “${current?.name}”` : prompt.kind === "sub" ? `New folder inside “${current?.name}”` : "New folder"}
          initial={prompt.kind === "rename" ? current?.name : ""}
          confirm={prompt.kind === "rename" ? "Rename" : "Create"}
          accounts={prompt.kind === "new" && multi ? accounts : undefined}
          defaultAccount={current?.account_email ?? selected?.account_email ?? accounts[0]?.email}
          onCancel={() => setPrompt(null)} onSubmit={folderAction} />
      )}
      {queueOpen && (
        <div className="sheet-backdrop" onClick={() => setQueueOpen(false)}>
          <div className="sheet evsheet" onClick={(e) => e.stopPropagation()}>
            <div className="sheet-bar"><span /><b>Changes waiting to sync</b><button onClick={() => setQueueOpen(false)}>Close</button></div>
            <div className="evbody">
              {queue.length === 0 && <div className="muted">Everything is synced.</div>}
              {queue.map((q) => (
                <div key={q.seq}>
                  <b>{q.what}</b> <span className="muted">· {q.failed ? "not accepted by the server" : q.tries ? `will retry (tried ${q.tries}×)` : "waiting"}</span>
                  {q.error && <div className="muted" style={{ fontSize: 12 }}>{q.error}</div>}
                </div>
              ))}
              {queue.length > 0 && !online && <div className="muted">You're offline. These will be sent when you reconnect.</div>}
            </div>
            <div className="sheet-bar">
              {rejected > 0 ? <button onClick={() => invoke("discard_failed").then(refreshQueue).catch((e) => flash(String(e)))}>Discard not accepted</button> : <span />}
              <button className="primary" onClick={async () => { try { await invoke("retry_pending"); flash("Sent"); } catch (e) { flash(String(e)); } refreshQueue(); loadFolders(); }}>Try again now</button>
            </div>
          </div>
        </div>
      )}
      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}
