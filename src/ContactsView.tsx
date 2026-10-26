import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import Icon from "./Icon";
import { Acct } from "./accounts";
import { initials } from "./recipients";
import { ContextMenu, MenuItem } from "./ContextMenu";
import { ContactActions, contactMenu, firstAddresses, MenuContact, vcfName } from "./contactMenu";
import { clickSelect, prune, selectAll, Selection, single } from "./selection";
import { adminApproval } from "./signIn";
import AdminApproval from "./AdminApproval";

export type ContactSource = "directory" | "personal" | "emailed" | "imported";
export type ContactRow = {
  id: string; source: ContactSource; account_email: string; name: string; email: string | null; subtitle: string | null; favourite?: boolean;
  /** "People you've emailed": messages exchanged and when the latest arrived (seconds). */
  message_count?: number | null; last_contacted?: number | null;
};
type Group = { total: number; rows: ContactRow[] };
export type ContactStatus = {
  email: string; granted: boolean;
  /** "ok" | "never" | "needs_consent" | "blocked" | "error" | "removed"; the directory is "n/a" for personal accounts */
  directory: string; personal: string;
  /** A personal Microsoft account (outlook.com, hotmail.com, live.com): no organisation directory. */
  personal_account?: boolean;
  directory_count: number; personal_count: number; last_sync: number | null;
};
export type Overview = { favourites?: Group; directory: Group; personal: Group; emailed?: Group; imported?: Group; accounts: ContactStatus[] };
export type ContactDetail = {
  id: string; source: ContactSource; account_email: string; folder: string | null; name: string;
  emails: string[]; phones: { kind: string; number: string }[];
  job_title: string | null; department: string | null; office: string | null; company: string | null;
  given_name?: string | null; surname?: string | null; favourite?: boolean;
  message_count?: number | null; last_contacted?: number | null;
};
export type ImportReport = { added: number; updated: number; skipped: number };

/** Which kind of account a group belongs to: Microsoft 365 (synced) or Gmail with an app password (kept locally). */
const GROUPS = [
  { key: "favourites", label: "Favourites", hint: "Contacts you marked as favourites", kind: "any" },
  { key: "directory", label: "Directory", hint: "Your organisation's address list", kind: "microsoft" },
  { key: "personal", label: "My contacts", hint: "Your own contacts", kind: "microsoft" },
  { key: "emailed", label: "People you've emailed", hint: "Built on this computer from the senders and recipients of your mail", kind: "imap" },
  { key: "imported", label: "Imported", hint: "Contacts imported from a vCard or Google Contacts CSV file", kind: "imap" },
] as const;
type GroupKey = (typeof GROUPS)[number]["key"];
export const GOOGLE_CONTACTS_NOTE = "Google contacts need Google sign-in, coming in a later update.";

const SOURCE_LABEL: Record<ContactSource, string> = { directory: "Directory", personal: "My contacts", emailed: "People you've emailed", imported: "Imported" };

const fmtDay = (ts: number) => new Date(ts * 1000).toLocaleDateString([], { day: "numeric", month: "short", year: "numeric" });
/** "5 messages · last 3 Oct 2026" for someone you've emailed. */
export function contactedLine(count: number | null | undefined, last: number | null | undefined) {
  const n = count ?? 0;
  const msgs = `${n} ${n === 1 ? "message" : "messages"}`;
  return last ? `${msgs} · last ${fmtDay(last)}` : msgs;
}
/** "12 contacts imported (3 updated, 1 skipped)". */
export function importLine(r: ImportReport) {
  const n = r.added + r.updated;
  const extra = [r.updated ? `${r.updated} updated` : "", r.skipped ? `${r.skipped} skipped` : ""].filter(Boolean).join(", ");
  return `${n} ${n === 1 ? "contact" : "contacts"} imported${extra ? ` (${extra})` : ""}`;
}

/** Why one account's contacts can't be shown, in words (null when nothing needs saying). */
export function statusNote(s: ContactStatus): { text: string; grant: boolean } | null {
  const parts = [s.directory, s.personal].filter((p) => p !== "n/a");
  if (parts.includes("needs_consent")) return { text: `Graphite Mail needs your permission to read contacts and your organisation's directory for ${s.email}.`, grant: true };
  if (parts.includes("blocked")) {
    const what = s.directory === "blocked" && s.personal === "blocked" ? "contacts and the directory" : s.directory === "blocked" ? "the directory" : "your contacts";
    return { text: `${s.email}: your organisation doesn't allow Graphite Mail to read ${what}. An administrator can allow it; then use Grant access again.`, grant: true };
  }
  if (parts.includes("removed")) return { text: `Cached contacts of ${s.email} were removed. Use Refresh contacts to download them again.`, grant: false };
  if (parts.includes("error")) return { text: `Contacts of ${s.email} couldn't be updated just now (offline?). Showing what is saved on this computer.`, grant: false };
  return null;
}

async function copy(text: string) {
  try { await navigator.clipboard.writeText(text); return true; } catch {
    const t = document.createElement("textarea");
    t.value = text; document.body.appendChild(t); t.select();
    const ok = document.execCommand("copy"); t.remove(); return ok;
  }
}

/** What the view asks the rest of the app to do (write, meet, search mail, block, make a rule). */
export type ContactHandlers = {
  onNewEmail: (to: string[], from: string) => void;
  onNewMeeting?: (attendees: string[], account: string) => void;
  onSearchMail?: (addresses: string[]) => void;
  onBlock?: (account: string, address: string) => void;
  onCreateRule?: (account: string, address: string) => void;
  onToast: (t: string) => void;
};

/** Contacts: Microsoft 365 accounts (favourites, the organisation's directory, the user's own contacts) and Gmail
 *  app-password accounts (people you've emailed, imported contacts; both kept on this computer). */
export default function ContactsView({ accounts, onNewEmail, onNewMeeting, onSearchMail, onBlock, onCreateRule, onToast }: { accounts: Acct[] } & ContactHandlers) {
  const ms = accounts.filter((a) => (a.provider ?? "microsoft") === "microsoft");
  const imap = accounts.filter((a) => a.provider === "imap");
  const usable = [...ms, ...imap];
  const [query, setQuery] = useState("");
  const [account, setAccount] = useState("");
  const [data, setData] = useState<Overview | null>(null);
  const [shut, setShut] = useState<Record<GroupKey, boolean>>({ favourites: false, directory: false, personal: false, emailed: false, imported: false });
  const [importing, setImporting] = useState(false);
  const [sel, setSel] = useState<Selection>({ ids: new Set(), anchor: null });
  const [detail, setDetail] = useState<ContactDetail | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [granting, setGranting] = useState("");
  const [error, setError] = useState("");
  const asked = useRef(0);
  const firstLoad = useRef(true);
  const list = useRef<HTMLDivElement>(null);

  const load = () => {
    const mine = ++asked.current;
    return invoke<Overview>("contacts_overview", { query, account: account || null })
      .then((o) => { if (mine === asked.current) setData(o); return o; })
      .catch((e) => { setError(String(e)); return null; });
  };

  async function refresh(quiet = false) {
    setRefreshing(true); setError("");
    try { await invoke("refresh_contacts", { account: account || null }); await load(); if (!quiet) onToast("Contacts updated"); }
    catch (e) { setError(String(e)); }
    finally { setRefreshing(false); }
  }

  useEffect(() => {
    if (!usable.length) return;
    const t = setTimeout(async () => {
      const o = await load();
      // The first visit downloads at once instead of waiting for the next background pass.
      if (firstLoad.current && o && o.accounts.some((s) => s.directory === "never" || s.personal === "never")) refresh(true);
      firstLoad.current = false;
    }, firstLoad.current ? 0 : 150);
    return () => clearTimeout(t);
  }, [query, account, usable.length]);

  // The groups that make sense for the accounts shown (all of them, or the one chosen in the account filter).
  const chosenKind = account ? (usable.find((a) => a.email === account)?.provider === "imap" ? "imap" : "microsoft") : null;
  // Personal Microsoft accounts have no organisation directory: the group is left out when none of the accounts shown has one.
  const statusesShown = (data?.accounts ?? []).filter((s) => !account || s.email.toLowerCase() === account.toLowerCase());
  const noDirectory = data !== null && statusesShown.length > 0 && statusesShown.every((s) => s.personal_account);
  const groups = GROUPS.filter((g) => g.kind === "any" || (chosenKind ? g.kind === chosenKind : g.kind === "microsoft" ? ms.length > 0 : imap.length > 0))
    .filter((g) => !(g.key === "directory" && noDirectory));

  // Every contact shown, in list order (a favourite appears twice; it counts once).
  const order = useMemo(() => {
    const seen = new Set<string>();
    const out: ContactRow[] = [];
    for (const g of groups) if (!shut[g.key]) for (const r of data?.[g.key]?.rows ?? []) if (!seen.has(r.id)) { seen.add(r.id); out.push(r); }
    return out;
  }, [data, shut, groups.length]);
  const ids = order.map((r) => r.id);
  useEffect(() => { setSel((s) => prune(ids, s)); }, [data]);

  const one = sel.ids.size === 1 ? [...sel.ids][0] : null;
  useEffect(() => {
    if (!one) { setDetail(null); return; }
    invoke<ContactDetail>("contact_detail", { id: one }).then(setDetail).catch((e) => { setDetail(null); setError(String(e)); });
  }, [one, data]);

  async function grant(email: string) {
    setGranting(email); setError("");
    try { await invoke("grant_contacts", { email }); await load(); onToast("Access granted"); }
    catch (e) { setError(String(e)); }
    finally { setGranting(""); }
  }

  /** "Import contacts…": a .vcf or Google Contacts .csv into the chosen (else the first) Gmail app-password account. */
  async function importContacts() {
    const target = imap.find((a) => a.email === account)?.email ?? imap[0]?.email;
    if (!target) return;
    const picked = await openDialog({ multiple: false, directory: false, title: "Import contacts", filters: [{ name: "Contacts (vCard, Google CSV)", extensions: ["vcf", "csv"] }] });
    const path = Array.isArray(picked) ? picked[0] : picked;
    if (!path) return;
    setImporting(true); setError("");
    try { const r = await invoke<ImportReport>("import_contacts", { account: target, path }); await load(); onToast(importLine(r)); }
    catch (e) { setError(String(e)); }
    finally { setImporting(false); }
  }

  const details = (list: string[]) => invoke<ContactDetail[]>("contact_details", { ids: list });
  const actions: ContactActions = {
    newEmail: (to, from) => onNewEmail(to, from),
    newMeeting: (to, acc) => onNewMeeting?.(to, acc),
    searchMail: (addrs) => onSearchMail?.(addrs),
    copy: async (text, note) => onToast((await copy(text)) ? note : "Couldn't copy"),
    favourite: async (list, on) => {
      try { for (const id of list) await invoke("set_contact_favourite", { id, on }); await load(); onToast(on ? "Added to favourites" : "Removed from favourites"); }
      catch (e) { setError(String(e)); }
    },
    exportVcf: async (cs) => {
      const path = await saveDialog({ title: "Export as vCard", defaultPath: vcfName(cs), filters: [{ name: "vCard", extensions: ["vcf"] }] });
      if (!path) return;
      try { const n = await invoke<number>("export_vcards", { ids: cs.map((c) => c.id), path }); onToast(n === 1 ? "Contact exported" : `${n} contacts exported`); }
      catch (e) { setError(String(e)); }
    },
    block: (acc, addr) => onBlock?.(acc, addr),
    createRule: (acc, addr) => onCreateRule?.(acc, addr),
  };

  /** The menu for the selection (after a right-click on `id`, which is selected first unless it already is). */
  async function openMenu(x: number, y: number, id?: string) {
    let chosen = ids.filter((x) => sel.ids.has(x)); // in list order
    if (id && !sel.ids.has(id)) { setSel(single(id)); chosen = [id]; }
    if (!chosen.length) return;
    try {
      const ds = await details(chosen);
      const cs: MenuContact[] = ds.map((d) => ({ id: d.id, account_email: d.account_email, name: d.name, emails: d.emails, phones: d.phones, favourite: !!d.favourite }));
      setMenu({ x, y, items: contactMenu(cs, actions) });
    } catch (e) { setError(String(e)); }
  }

  function onListKey(e: React.KeyboardEvent) {
    if ((e.target as HTMLElement).closest("input, select, textarea, button")) return;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a" && ids.length) { e.preventDefault(); setSel(selectAll(ids)); }
    else if (e.key === "Escape" && sel.ids.size) { e.preventDefault(); setSel({ ids: new Set(), anchor: null }); }
    else if ((e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) && sel.ids.size) {
      e.preventDefault();
      const r = list.current?.querySelector(".crow.active")?.getBoundingClientRect();
      openMenu(r ? r.left + 40 : 300, r ? r.top + r.height / 2 : 200);
    }
  }

  if (!usable.length) {
    return (
      <section className="contacts-only">
        <div className="empty"><Icon name="contacts" size={28} /><p>Contacts are available for Microsoft 365 accounts and Gmail (app password) accounts.</p></div>
      </section>
    );
  }

  const notes = (data?.accounts ?? []).map((s) => ({ s, note: statusNote(s) })).filter((x) => x.note);
  const total = groups.filter((g) => g.key !== "favourites").reduce((n, g) => n + (data?.[g.key]?.total ?? 0), 0);
  const showImap = imap.length > 0 && chosenKind !== "microsoft";
  const picking = sel.ids.size > 1;
  const chosenRows = order.filter((r) => sel.ids.has(r.id));

  return (
    <>
      <section className="list contacts-list" aria-label="Contacts">
        <div className="toolbar" data-tauri-drag-region>
          <div className="title" data-tauri-drag-region>
            <b>Contacts</b>
            <span>{data ? `${total} ${query ? "matches" : "people"}` : "Loading…"}{picking ? ` · ${sel.ids.size} selected` : ""}</span>
          </div>
          {showImap && (
            <button className="icon" onClick={importContacts} disabled={importing} title="Import contacts (vCard or Google Contacts CSV)" aria-label="Import contacts">
              <Icon name="import" />
            </button>
          )}
          <button className="icon" onClick={() => refresh()} disabled={refreshing} title="Refresh contacts" aria-label="Refresh contacts">
            <span className={refreshing ? "spin" : ""}><Icon name="refresh" /></span>
          </button>
        </div>
        <div className="search">
          <input type="search" placeholder="Search contacts" aria-label="Search contacts" value={query} onChange={(e) => setQuery(e.target.value)} />
        </div>
        {usable.length > 1 && (
          <div className="search">
            <select aria-label="Account" value={account} onChange={(e) => { setAccount(e.target.value); setSel({ ids: new Set(), anchor: null }); }} style={{ width: "100%" }}>
              <option value="">{imap.length ? "All accounts" : "All Microsoft 365 accounts"}</option>
              {usable.map((a) => <option key={a.email} value={a.email}>{a.email}</option>)}
            </select>
          </div>
        )}
        <div className={`scroller ${picking ? "picking" : ""}`} ref={list} tabIndex={0} onKeyDown={onListKey} aria-label="Contact list">
          {notes.map(({ s, note }) => (
            <div key={s.email} className="notice cnotice" role="status">
              <span>{note!.text}</span>
              {note!.grant && <button className="primary" disabled={!!granting} onClick={() => grant(s.email)}>{granting === s.email ? "Waiting for browser…" : "Grant access"}</button>}
            </div>
          ))}
          {showImap && (
            <div className="notice cnotice" role="note">
              <span>{imap.some((a) => !a.imap) ? `${GOOGLE_CONTACTS_NOTE} Until then, people` : "Your provider's own address book isn't synced for IMAP accounts. People"} you've emailed are listed from your mail, and you can import a vCard or Google Contacts CSV file.</span>
              <button disabled={importing} onClick={importContacts}>{importing ? "Importing…" : "Import contacts…"}</button>
            </div>
          )}
          {error && (adminApproval(error) ? <AdminApproval message={adminApproval(error)!} email={granting || account || undefined} /> : <div className="error" role="alert" style={{ padding: "4px 8px" }}>{error}</div>)}
          {groups.map((g) => {
            const grp = data?.[g.key];
            if (g.key === "favourites" && !grp?.total) return null; // shown once there are favourites
            const open = !shut[g.key];
            return (
              <div key={g.key} className="cgroup" role="group" aria-label={g.label}>
                <button className="cgroup-head" aria-expanded={open} title={g.hint} onClick={() => setShut((s) => ({ ...s, [g.key]: !s[g.key] }))}>
                  <span className="chev">{open ? "▾" : "▸"}</span><span className="cgroup-name">{g.label}</span><span className="count">{grp?.total ?? 0}</span>
                </button>
                {open && (
                  <div role="listbox" aria-label={`${g.label} list`} aria-multiselectable="true">
                    {grp?.rows.map((r) => (
                      <div key={r.id} role="option" aria-selected={sel.ids.has(r.id)} className={`crow ${sel.ids.has(r.id) ? "active" : ""}`}
                        onClick={(e) => setSel((s) => (e.ctrlKey || e.metaKey || e.shiftKey ? clickSelect(ids, s, r.id, { ctrl: e.ctrlKey || e.metaKey, shift: e.shiftKey }) : single(r.id)))}
                        onContextMenu={(e) => { e.preventDefault(); openMenu(e.clientX, e.clientY, r.id); }}>
                        <input type="checkbox" className="ccheck" checked={sel.ids.has(r.id)} aria-label={`Select ${r.name}`} tabIndex={-1}
                          onClick={(e) => e.stopPropagation()} onChange={() => setSel((s) => clickSelect(ids, s, r.id, { ctrl: true }))} />
                        <div className="avatar">{initials(r.name)}</div>
                        <div className="crow-text">
                          <div className="cname">{r.name}{r.favourite && <span className="cstar" title="Favourite" aria-label="Favourite"> ★</span>}</div>
                          <div className="csub">{r.source === "emailed" ? `${r.email ?? ""} · ${contactedLine(r.message_count, r.last_contacted)}` : r.subtitle ?? r.email ?? ""}</div>
                        </div>
                      </div>
                    ))}
                    {grp && grp.total === 0 && <div className="muted cnone">{query ? "No matches" : "No contacts here yet"}</div>}
                    {grp && grp.total > grp.rows.length && <div className="muted cnone">Showing {grp.rows.length} of {grp.total}. Search to find the others.</div>}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </section>

      <section className="reader contact-detail" aria-label="Contact details">
        {picking ? (
          <div className="multisel">
            <b>{sel.ids.size} contacts selected</b>
            <div className="rowbtns" style={{ justifyContent: "center" }}>
              {(() => { const to = firstAddresses(chosenRows.filter((r) => r.email).map((r) => ({ id: r.id, account_email: r.account_email, name: r.name, emails: [r.email!], phones: [], favourite: false })));
                return to.length > 0 && <>
                  <button className="primary" onClick={() => onNewEmail(to, chosenRows.find((r) => r.email)!.account_email)}>New email to all</button>
                  <button onClick={() => actions.copy(to.join("; "), `${to.length} addresses copied`)}>Copy all addresses</button>
                </>; })()}
            </div>
            <span style={{ fontSize: 12 }}>Right-click for more · Ctrl+click or Shift+click to change the selection · Esc to clear</span>
          </div>
        ) : detail ? (
          <div className="cdetail" onContextMenu={(e) => { e.preventDefault(); openMenu(e.clientX, e.clientY, detail.id); }}>
            <header className="cdetail-head">
              <div className="avatar big">{initials(detail.name)}</div>
              <div>
                <h2>{detail.name}{detail.favourite && <span className="cstar" aria-label="Favourite"> ★</span>}</h2>
                {(detail.job_title || detail.department) && <div className="muted">{[detail.job_title, detail.department].filter(Boolean).join(" · ")}</div>}
              </div>
            </header>
            <div className="rowbtns">
              <button className="primary" disabled={!detail.emails.length} onClick={() => onNewEmail([detail.emails[0]], detail.account_email)}><Icon name="compose" /> New email</button>
              <button disabled={!detail.emails.length} onClick={async () => onToast((await copy(detail.emails[0])) ? "Email address copied" : "Couldn't copy")}><Icon name="copy" /> Copy email</button>
            </div>
            <dl className="cfields">
              {detail.emails.length > 0 && <><dt>Email</dt><dd>{detail.emails.map((e) => <div key={e}>{e}</div>)}</dd></>}
              {detail.phones.length > 0 && <><dt>Phone</dt><dd>{detail.phones.map((p) => <div key={p.number}>{p.number} <span className="muted">({p.kind})</span></div>)}</dd></>}
              {detail.job_title && <><dt>Job title</dt><dd>{detail.job_title}</dd></>}
              {detail.department && <><dt>Department</dt><dd>{detail.department}</dd></>}
              {detail.office && <><dt>Office</dt><dd>{detail.office}</dd></>}
              {detail.company && <><dt>Company</dt><dd>{detail.company}</dd></>}
              {detail.source === "emailed" && <><dt>Messages</dt><dd>{detail.message_count ?? 0}</dd></>}
              {detail.source === "emailed" && detail.last_contacted && <><dt>Last contacted</dt><dd>{fmtDay(detail.last_contacted)}</dd></>}
            </dl>
            <div className="muted csource">
              {detail.source === "personal" ? `My contacts${detail.folder ? ` · ${detail.folder}` : ""}` : SOURCE_LABEL[detail.source]}{usable.length > 1 ? ` · ${detail.account_email}` : ""}
            </div>
          </div>
        ) : (
          <div className="empty">No contact selected</div>
        )}
      </section>
      {menu && <ContextMenu x={menu.x} y={menu.y} items={menu.items} label="Contact actions" onClose={() => { setMenu(null); list.current?.focus(); }} />}
    </>
  );
}
