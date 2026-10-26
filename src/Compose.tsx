import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as pickFiles } from "@tauri-apps/plugin-dialog";
import RecipientInput from "./RecipientInput";
import { sanitizedFragment } from "./emailHtml";

export type ComposeInit = {
  mode: "new" | "reply" | "replyAll" | "forward";
  replyTo?: string;
  to?: string[];
  cc?: string[];
  /** Blind copies. Never carried over from a received message (replies and forwards start without any). */
  bcc?: string[];
  subject?: string;
  /** The message being answered (untrusted email HTML: sanitised and inserted as nodes) and its header (plain text). */
  quoteHtml?: string;
  quoteHeader?: string;
  /** The user loaded the quoted message's remote images, so the quote may keep them. */
  quoteImages?: boolean;
  /** Account (email) to send from. */
  from?: string;
  /** Set when reopening a saved draft: its id, the editor's saved HTML and files. */
  draftId?: string;
  html?: string;
  attachments?: string[];
};
type Acct = { email: string; display_name: string };

const split = (s: string) => s.split(/[,;\s]+/).map((x) => x.trim()).filter(Boolean);
const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/\n/g, "<br>");
const sigBlock = (text: string) => (text.trim() ? `<div data-sig="1">${esc(text.trim())}</div>` : "");
const valid = (e: string) => /^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(e);

export default function Compose({ init, accounts, onClose, onSent }: { init: ComposeInit; accounts: Acct[]; onClose: () => void; onSent: (queued: boolean) => void }) {
  const [from, setFrom] = useState(init.from ?? accounts[0]?.email ?? "");
  const [to, setTo] = useState((init.to ?? []).join(", "));
  const [cc, setCc] = useState((init.cc ?? []).join(", "));
  const [bcc, setBcc] = useState((init.bcc ?? []).join(", "));
  // The Bcc row is hidden until asked for (or when a draft already has blind copies).
  const [showBcc, setShowBcc] = useState((init.bcc ?? []).length > 0);
  const [subject, setSubject] = useState(init.subject ?? "");
  const [error, setError] = useState("");
  const [sending, setSending] = useState(false);
  const [files, setFiles] = useState<string[]>(init.attachments ?? []);
  const [draftId, setDraftId] = useState<string | undefined>(init.draftId);
  const [confirmClose, setConfirmClose] = useState(false);
  const [savedNote, setSavedNote] = useState("");
  const changed = useRef(false);
  const body = useRef<HTMLDivElement>(null);
  const touch = () => { changed.current = true; };

  useEffect(() => {
    (async () => {
      if (body.current) {
        if (init.html !== undefined) {
          // A saved draft comes back as it was, minus anything a sanitiser would remove (it can quote an email).
          body.current.replaceChildren(sanitizedFragment(init.html, true));
        } else {
          const sig = sigBlock(await invoke<string>("get_signature", { email: from }).catch(() => ""));
          if (!body.current) return; // the editor was closed while the signature was being fetched
          body.current.innerHTML = `<p><br></p>${sig}`; // our own markup; the signature text is escaped
          if (init.quoteHtml) {
            const para = () => { const p = document.createElement("p"); p.append(document.createElement("br")); return p; };
            const header = document.createElement("p");
            header.style.color = "#888";
            header.textContent = init.quoteHeader ?? "";
            const quote = document.createElement("blockquote");
            quote.setAttribute("style", "margin:0 0 0 8px;padding-left:10px;border-left:3px solid #ccc");
            quote.append(sanitizedFragment(init.quoteHtml, !!init.quoteImages));
            body.current.append(para(), document.createElement("hr"), header, quote);
          }
        }
      }
      (init.mode === "forward" || !init.to?.length ? document.getElementById("cmp-to") : body.current)?.focus();
    })();
  }, []);

  // Another "From" account means another signature.
  async function changeFrom(email: string) {
    setFrom(email);
    const holder = body.current?.querySelector("[data-sig]");
    if (holder) {
      holder.innerHTML = esc((await invoke<string>("get_signature", { email }).catch(() => "")).trim());
    } else if (body.current && changed.current === false) {
      const sig = sigBlock(await invoke<string>("get_signature", { email }).catch(() => ""));
      if (sig) body.current.insertAdjacentHTML("afterbegin", `<p><br></p>${sig}`);
    }
  }

  async function saveDraft(quiet = false) {
    try {
      const id = await invoke<string>("save_draft", {
        draft: { id: draftId ?? null, mode: init.mode, reply_to: init.replyTo ?? null, from: from || null, to, cc, bcc, subject, html: body.current?.innerHTML ?? "", attachments: files },
      });
      setDraftId(id);
      changed.current = false;
      setSavedNote(quiet ? "Draft saved" : "");
      return true;
    } catch (e) { setError(String(e)); return false; }
  }

  // Saved automatically every 20 seconds while you write.
  useEffect(() => {
    const t = setInterval(() => { if (changed.current && !sending) saveDraft(true); }, 20000);
    return () => clearInterval(t);
  });

  function requestClose() {
    if (changed.current) setConfirmClose(true);
    else onClose();
  }
  async function discard() {
    if (draftId) await invoke("delete_draft", { id: draftId }).catch(() => {});
    onClose();
  }

  async function send() {
    const toList = split(to), ccList = split(cc), bccList = split(bcc);
    const bad = [...toList, ...ccList, ...bccList].filter((e) => !valid(e));
    if (!toList.length && !ccList.length && !bccList.length) return setError("Add at least one recipient.");
    if (bad.length) return setError(`Check these addresses: ${bad.join(", ")}`);
    setSending(true);
    try {
      const html = `<div style="font-family:Calibri,Arial,sans-serif;font-size:11pt">${body.current?.innerHTML ?? ""}</div>`;
      const sent = await invoke<boolean>("send_mail", {
        draft: { mode: init.mode, reply_to: init.replyTo ?? null, from: from || null, to: toList, cc: ccList, bcc: bccList, subject, html, attachments: files },
      });
      if (draftId) await invoke("delete_draft", { id: draftId }).catch(() => {});
      onSent(!sent);
    } catch (e) {
      setError(String(e));
      setSending(false);
    }
  }

  const cmd = (c: string) => document.execCommand(c);
  const nameOf = (p: string) => p.split(/[\\/]/).pop() ?? p;
  async function attach() {
    const picked = await pickFiles({ multiple: true, title: "Attach files" });
    const list = Array.isArray(picked) ? picked : picked ? [picked] : [];
    touch();
    setFiles((f) => [...f, ...list.filter((p) => !f.includes(p))]);
  }

  return (
    <div className="sheet-backdrop" onKeyDown={(e) => { if (e.key === "Escape") requestClose(); if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) send(); }}>
      <div className="sheet">
        <div className="sheet-bar">
          <button onClick={requestClose}>Cancel</button>
          <b>{subject || "New Message"}</b>
          <button className="primary" onClick={send} disabled={sending}>{sending ? "Sending…" : "Send"}</button>
        </div>
        {accounts.length > 1 && (
          <label className="field"><span>From:</span>
            <select value={from} onChange={(e) => changeFrom(e.target.value)} disabled={!!init.replyTo} title={init.replyTo ? "Replies and forwards are sent from the account that received the message" : undefined}>
              {accounts.map((a) => <option key={a.email} value={a.email}>{a.display_name} &lt;{a.email}&gt;</option>)}
            </select>
          </label>
        )}
        <label className="field"><span>To:</span><RecipientInput id="cmp-to" label="To" account={from} value={to} onChange={(v) => { touch(); setTo(v); }} /></label>
        <label className="field"><span>Cc:</span><RecipientInput id="cmp-cc" label="Cc" account={from} value={cc} onChange={(v) => { touch(); setCc(v); }} />
          {!showBcc && <button type="button" className="linkish fieldlink" onClick={(e) => { e.preventDefault(); setShowBcc(true); setTimeout(() => document.getElementById("cmp-bcc")?.focus()); }} title="Add blind copies: recipients others can't see">Bcc</button>}
        </label>
        {showBcc && <label className="field"><span>Bcc:</span><RecipientInput id="cmp-bcc" label="Bcc" account={from} value={bcc} onChange={(v) => { touch(); setBcc(v); }} /></label>}
        <label className="field"><span>Subject:</span><input value={subject} onChange={(e) => { touch(); setSubject(e.target.value); }} /></label>
        <div className="fmt">
          <button className="icon" onClick={() => cmd("bold")}><b>B</b></button>
          <button className="icon" onClick={() => cmd("italic")}><i>I</i></button>
          <button className="icon" onClick={() => cmd("underline")}><u>U</u></button>
          <button className="icon" onClick={() => cmd("insertUnorderedList")}>•</button>
          <span style={{ flex: 1 }} />
          <button onClick={attach} title="Attach files (up to 15 MB in total)">📎 Attach</button>
        </div>
        {files.length > 0 && (
          <div className="attrow" style={{ padding: "6px 16px" }}>
            {files.map((p) => (
              <span key={p} className="attchip"><span className="attname" style={{ padding: "3px 4px 3px 10px" }}>{nameOf(p)}</span>
                <button className="attsave" title="Remove" onClick={() => { touch(); setFiles((f) => f.filter((x) => x !== p)); }}>✕</button></span>
            ))}
          </div>
        )}
        <div className="editor" contentEditable ref={body} suppressContentEditableWarning onInput={touch} />
        {savedNote && !confirmClose && <div className="muted" style={{ padding: "0 16px 8px", fontSize: 12 }}>{savedNote}</div>}
        {error && <div className="error" style={{ padding: "0 16px 12px" }}>{error}</div>}
        {confirmClose && (
          <div className="notice" style={{ margin: "0 16px 14px" }}>
            <span>Save this message as a draft?</span>
            <span className="rowbtns">
              <button className="primary" onClick={async () => { if (await saveDraft()) onClose(); }}>Save draft</button>
              <button onClick={discard}>Discard</button>
              <button onClick={() => setConfirmClose(false)}>Keep editing</button>
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
