import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ask } from "@tauri-apps/plugin-dialog";
import MessageFrame from "./MessageFrame";
import { buildDoc } from "./emailHtml";
import Icon from "./Icon";

export type Cal = { id: string; name: string; color: string; is_primary: boolean; can_edit: boolean; account_email: string; provider?: string };
type Ev = {
  id: string; calendar_id: string; title: string; description_html: string | null; location: string | null;
  online_meeting_url: string | null; start_utc: number; end_utc: number; is_all_day: boolean;
  organizer_json: string | null; attendees_json: string | null; my_response: string; color: string; pending: boolean;
};

const DAY = 86400;
const HOUR_PX = 48;
const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate());
const addDays = (d: Date, n: number) => new Date(d.getFullYear(), d.getMonth(), d.getDate() + n);
const startOfWeek = (d: Date) => addDays(startOfDay(d), -((d.getDay() + 6) % 7)); // Monday
const sameDay = (a: Date, b: Date) => a.toDateString() === b.toDateString();
const ts = (d: Date) => Math.floor(d.getTime() / 1000);
const fmtTime = (t: number) => new Date(t * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

/** All-day events come back as UTC midnights; read their date in UTC so they don't shift a day. */
function evDays(e: Ev): [Date, Date] {
  if (e.is_all_day) {
    const s = new Date(e.start_utc * 1000), en = new Date(e.end_utc * 1000);
    return [new Date(s.getUTCFullYear(), s.getUTCMonth(), s.getUTCDate()), new Date(en.getUTCFullYear(), en.getUTCMonth(), en.getUTCDate())];
  }
  return [new Date(e.start_utc * 1000), new Date(e.end_utc * 1000)];
}
const onDay = (e: Ev, day: Date) => {
  const [s, en] = evDays(e);
  const d0 = startOfDay(day), d1 = addDays(d0, 1);
  return s < d1 && (e.is_all_day ? en > d0 : en > d0 || sameDay(s, day));
};

export default function CalendarView({ calendars, hidden, onToast, newRequest }: { calendars: Cal[]; hidden: Set<string>; onToast: (t: string) => void; newRequest?: { teams: boolean; n: number; attendees?: string[]; account?: string } }) {
  const [mode, setMode] = useState<"week" | "month">(() => (localStorage.getItem("calMode") as "week" | "month") || "week");
  const [anchor, setAnchor] = useState(startOfDay(new Date()));
  const [events, setEvents] = useState<Ev[]>([]);
  const [sel, setSel] = useState<Ev | null>(null);
  const [creating, setCreating] = useState<{ start: Date; allDay: boolean; teams?: boolean; attendees?: string[]; account?: string } | null>(null);
  // The ribbon's "New Meeting" / "New Event" buttons ask for the editor via newRequest.
  useEffect(() => { if (newRequest && newRequest.n > 0) setCreating({ start: nextHalfHour(), allDay: false, teams: newRequest.teams, attendees: newRequest.attendees, account: newRequest.account }); }, [newRequest?.n]);
  const grid = useRef<HTMLDivElement>(null);

  const range = useMemo(() => {
    if (mode === "week") { const s = startOfWeek(anchor); return [s, addDays(s, 7)]; }
    const first = new Date(anchor.getFullYear(), anchor.getMonth(), 1);
    const s = startOfWeek(first);
    return [s, addDays(s, 42)];
  }, [mode, anchor]);

  const load = () => invoke<Ev[]>("list_events", { start: ts(range[0]) - DAY, end: ts(range[1]) + DAY }).then(setEvents).catch((e) => { invoke("log_js", { message: `list_events failed: ${e}` }); onToast(String(e)); });
  useEffect(() => { load(); }, [range[0].getTime(), mode]);
  useEffect(() => { try { localStorage.setItem("calMode", mode); } catch { /* ignore */ } }, [mode]);
  useEffect(() => { if (mode === "week" && grid.current) grid.current.scrollTop = HOUR_PX * 7.5; }, [mode]);
  useEffect(() => { const t = setInterval(load, 60000); return () => clearInterval(t); });

  const visible = events.filter((e) => !hidden.has(e.calendar_id));
  const step = (n: number) => setAnchor(mode === "week" ? addDays(anchor, 7 * n) : new Date(anchor.getFullYear(), anchor.getMonth() + n, 1));
  const title = mode === "week"
    ? `${range[0].toLocaleDateString([], { month: "long", year: "numeric" })}`
    : anchor.toLocaleDateString([], { month: "long", year: "numeric" });

  const respond = async (e: Ev, r: string) => {
    try { await invoke("rsvp_event", { id: e.id, response: r }); onToast("Response sent"); setSel(null); load(); } catch (x) { onToast(String(x)); }
  };
  const remove = async (e: Ev) => {
    if (!(await ask(`Delete "${e.title}"?`, { title: "Delete event", kind: "warning" }))) return;
    try { await invoke("delete_event", { id: e.id }); setSel(null); load(); } catch (x) { onToast(String(x)); }
  };

  const days = Array.from({ length: mode === "week" ? 7 : 42 }, (_, i) => addDays(range[0], i));
  const today = new Date();

  return (
    <section className="calendar">
      <div className="toolbar" data-tauri-drag-region>
        <div className="title" data-tauri-drag-region><b>{title}</b></div>
        <div className="seg">
          <button className={mode === "week" ? "on" : ""} onClick={() => setMode("week")}>Week</button>
          <button className={mode === "month" ? "on" : ""} onClick={() => setMode("month")}>Month</button>
        </div>
        <button className="icon" onClick={() => step(-1)} title="Previous">‹</button>
        <button onClick={() => setAnchor(startOfDay(new Date()))}>Today</button>
        <button className="icon" onClick={() => step(1)} title="Next">›</button>
        <button className="icon" onClick={() => setCreating({ start: nextHalfHour(), allDay: false })} title="New event"><Icon name="compose" /></button>
      </div>

      {mode === "week" ? (
        <div className="week">
          <div className="week-head">
            <div className="gutter" />
            {days.map((d) => (
              <div key={d.toISOString()} className={`dayhead ${sameDay(d, today) ? "today" : ""}`}>
                <span>{d.toLocaleDateString([], { weekday: "short" })}</span> <b>{d.getDate()}</b>
              </div>
            ))}
          </div>
          <div className="allday-row">
            <div className="gutter">all-day</div>
            {days.map((d) => (
              <div key={d.toISOString()} className="allday-cell" onDoubleClick={() => setCreating({ start: d, allDay: true })}>
                {visible.filter((e) => e.is_all_day && onDay(e, d)).map((e) => (
                  <div key={e.id} className="chip" style={{ background: e.color }} onClick={() => setSel(e)}>{e.title}</div>
                ))}
              </div>
            ))}
          </div>
          <div className="week-grid" ref={grid}>
            <div className="hours">
              {Array.from({ length: 24 }, (_, h) => <div key={h} style={{ height: HOUR_PX }}><span>{h ? new Date(2000, 0, 1, h).toLocaleTimeString([], { hour: "numeric" }) : ""}</span></div>)}
            </div>
            {days.map((d) => {
              const dayEvents = layout(visible.filter((e) => !e.is_all_day && onDay(e, d)), d);
              return (
                <div key={d.toISOString()} className="daycol" style={{ height: HOUR_PX * 24 }}
                  onDoubleClick={(ev) => {
                    const y = ev.clientY - (ev.currentTarget as HTMLElement).getBoundingClientRect().top;
                    const mins = Math.floor((y / HOUR_PX) * 2) * 30;
                    setCreating({ start: new Date(d.getFullYear(), d.getMonth(), d.getDate(), 0, mins), allDay: false });
                  }}>
                  {sameDay(d, today) && <div className="nowline" style={{ top: ((today.getHours() * 60 + today.getMinutes()) / 60) * HOUR_PX }} />}
                  {dayEvents.map(({ e, top, height, col, cols }) => (
                    <div key={e.id} className={`ev ${e.my_response === "declined" ? "declined" : ""} ${e.my_response === "notResponded" || e.my_response === "none" ? "tentative" : ""}`}
                      style={{ top, height: Math.max(height, 20), left: `calc(${(col / cols) * 100}% + 2px)`, width: `calc(${100 / cols}% - 4px)`, ["--c" as string]: e.color }}
                      onClick={(x) => { x.stopPropagation(); setSel(e); }}>
                      <b>{e.title}</b>
                      {height > 34 && <span>{fmtTime(e.start_utc)}{e.location ? ` · ${e.location}` : ""}</span>}
                    </div>
                  ))}
                </div>
              );
            })}
          </div>
        </div>
      ) : (
        <div className="month">
          {["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].map((w) => <div key={w} className="mhead">{w}</div>)}
          {days.map((d) => {
            const list = visible.filter((e) => onDay(e, d));
            return (
              <div key={d.toISOString()} className={`mcell ${d.getMonth() !== anchor.getMonth() ? "out" : ""}`} onDoubleClick={() => setCreating({ start: new Date(d.getFullYear(), d.getMonth(), d.getDate(), 9), allDay: false })}>
                <div className={`mday ${sameDay(d, today) ? "today" : ""}`}><span>{d.getDate()}</span></div>
                {list.slice(0, 3).map((e) => (
                  <div key={e.id} className="mev" onClick={() => setSel(e)}>
                    <i style={{ background: e.color }} />{!e.is_all_day && <span className="muted">{fmtTime(e.start_utc)}</span>} {e.title}
                  </div>
                ))}
                {list.length > 3 && <div className="more" onClick={() => { setMode("week"); setAnchor(d); }}>+{list.length - 3} more</div>}
              </div>
            );
          })}
        </div>
      )}

      {sel && <EventDetail e={sel} cal={calendars.find((c) => c.id === sel.calendar_id)} onClose={() => setSel(null)} onRespond={respond} onDelete={remove} />}
      {creating && <NewEvent init={creating} calendars={calendars.filter((c) => c.can_edit)} onClose={() => setCreating(null)}
        onCreated={() => { setCreating(null); onToast("Event saved"); load(); }} onError={onToast} />}
    </section>
  );
}

function nextHalfHour() {
  const d = new Date();
  d.setMinutes(d.getMinutes() < 30 ? 30 : 60, 0, 0);
  return d;
}

/** Side-by-side columns for overlapping events within one day. */
function layout(list: Ev[], day: Date) {
  const d0 = ts(startOfDay(day));
  const items = list
    .map((e) => {
      const s = Math.max(e.start_utc, d0), en = Math.min(e.end_utc, d0 + DAY);
      return { e, s, en, top: ((s - d0) / 3600) * HOUR_PX, height: ((en - s) / 3600) * HOUR_PX, col: 0, cols: 1 };
    })
    .sort((a, b) => a.s - b.s || b.en - a.en);
  let group: typeof items = [], groupEnd = 0;
  const flush = () => { const n = Math.max(...group.map((g) => g.col)) + 1; group.forEach((g) => (g.cols = n)); group = []; };
  for (const it of items) {
    if (group.length && it.s >= groupEnd) flush();
    const used = new Set(group.filter((g) => g.en > it.s).map((g) => g.col));
    let c = 0; while (used.has(c)) c++;
    it.col = c; group.push(it); groupEnd = Math.max(groupEnd, it.en);
  }
  if (group.length) flush();
  return items;
}

function EventDetail({ e, cal, onClose, onRespond, onDelete }: { e: Ev; cal?: Cal; onClose: () => void; onRespond: (e: Ev, r: string) => void; onDelete: (e: Ev) => void }) {
  const safe = <T,>(j: string | null, d: T): T => { try { return (j ? JSON.parse(j) : d) ?? d; } catch { return d; } };
  const attendees = safe<{ name?: string; email?: string; response?: string }[]>(e.attendees_json, []);
  const org = safe<{ name?: string; address?: string }>(e.organizer_json, {});
  const isOrganizer = e.my_response === "organizer";
  const [s, en] = evDays(e);
  const when = e.is_all_day
    ? `${s.toLocaleDateString([], { weekday: "long", day: "numeric", month: "long" })}${en.getTime() - s.getTime() > DAY * 1000 ? ` – ${addDays(en, -1).toLocaleDateString([], { day: "numeric", month: "long" })}` : ""} · All day`
    : `${s.toLocaleDateString([], { weekday: "long", day: "numeric", month: "long" })} · ${fmtTime(e.start_utc)} – ${fmtTime(e.end_utc)}`;
  const label: Record<string, string> = { accepted: "Accepted", tentativelyAccepted: "Tentative", declined: "Declined", notResponded: "Not responded", none: "Not responded", organizer: "You're the organizer" };
  return (
    <div className="sheet-backdrop" onClick={onClose} onKeyDown={(k) => k.key === "Escape" && onClose()}>
      <div className="sheet evsheet" onClick={(x) => x.stopPropagation()}>
        <div className="evhead" style={{ borderColor: e.color }}>
          <h2>{e.title}</h2>
          <div className="muted">{when}</div>
          {e.location && <div className="muted">{e.location}</div>}
          <div className="muted" style={{ marginTop: 4 }}><i className="caldot" style={{ background: e.color }} />{cal?.name}{e.pending ? " · waiting to sync" : ""}</div>
        </div>
        <div className="evbody">
          {e.online_meeting_url && <button className="primary" onClick={() => invoke("open_link", { url: e.online_meeting_url! })}>{/meet\.google\.com/.test(e.online_meeting_url) ? "Join Google Meet" : "Join Teams meeting"}</button>}
          {!isOrganizer && (
            <div className="rsvp">
              <span className="muted">{label[e.my_response] ?? e.my_response}</span>
              <button onClick={() => onRespond(e, "accept")}>Accept</button>
              <button onClick={() => onRespond(e, "tentativelyAccept")}>Maybe</button>
              <button onClick={() => onRespond(e, "decline")}>Decline</button>
            </div>
          )}
          {(org.name || org.address) && <div><span className="muted">Organizer: </span>{org.name || org.address}</div>}
          {attendees.length > 0 && (
            <div className="people">
              <div className="muted">{attendees.length} invitees</div>
              {attendees.slice(0, 30).map((a, i) => (
                <div key={i}>{a.name || a.email} <span className="muted">{a.response && a.response !== "none" ? `· ${label[a.response] ?? a.response}` : ""}</span></div>
              ))}
            </div>
          )}
          {e.description_html && <div className="evdesc evframe"><MessageFrame doc={buildDoc(e.description_html, false, false).doc} title="event description" /></div>}
        </div>
        <div className="sheet-bar">
          {cal?.can_edit !== false ? <button onClick={() => onDelete(e)}>Delete</button> : <span />}
          <button onClick={onClose}>Close</button>
        </div>
      </div>
    </div>
  );
}

const toLocalInput = (d: Date, dateOnly: boolean) => {
  const p = (n: number) => String(n).padStart(2, "0");
  const date = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  return dateOnly ? date : `${date}T${p(d.getHours())}:${p(d.getMinutes())}`;
};

function NewEvent({ init, calendars, onClose, onCreated, onError }: { init: { start: Date; allDay: boolean; teams?: boolean; attendees?: string[]; account?: string }; calendars: Cal[]; onClose: () => void; onCreated: () => void; onError: (t: string) => void }) {
  const [title, setTitle] = useState("");
  const [allDay, setAllDay] = useState(init.allDay);
  const [start, setStart] = useState(toLocalInput(init.start, init.allDay));
  const [end, setEnd] = useState(toLocalInput(init.allDay ? init.start : new Date(init.start.getTime() + 3600e3), init.allDay));
  // A meeting started from a contact goes in that account's calendar.
  const mine = calendars.filter((c) => !init.account || c.account_email.toLowerCase() === init.account.toLowerCase());
  const [calId, setCalId] = useState((mine.find((c) => c.is_primary) ?? mine[0] ?? calendars.find((c) => c.is_primary) ?? calendars[0])?.id ?? "");
  const [location, setLocation] = useState("");
  const [people, setPeople] = useState((init.attendees ?? []).join(", "));
  const [teams, setTeams] = useState(!!init.teams);
  const [notes, setNotes] = useState("");
  const [err, setErr] = useState("");

  const switchAllDay = (v: boolean) => {
    setAllDay(v);
    setStart(toLocalInput(new Date(start), v));
    setEnd(toLocalInput(new Date(v ? start : new Date(new Date(start).getTime() + 3600e3)), v));
  };

  async function save() {
    if (!title.trim()) return setErr("Add a title.");
    let s: number, e: number;
    if (allDay) {
      const [y1, m1, d1] = start.split("-").map(Number), [y2, m2, d2] = end.split("-").map(Number);
      s = Date.UTC(y1, m1 - 1, d1) / 1000; e = Date.UTC(y2, m2 - 1, d2) / 1000 + DAY;
    } else { s = ts(new Date(start)); e = ts(new Date(end)); }
    if (!(e > s)) return setErr("End must be after start.");
    const attendees = people.split(/[,;\s]+/).filter(Boolean);
    if (attendees.some((a) => !/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(a))) return setErr("Check the invitee email addresses.");
    try {
      await invoke("create_event", { event: { calendar_id: calId, title, start_utc: s, end_utc: e, is_all_day: allDay, location: location || null, attendees, body: notes ? notes.replace(/\n/g, "<br>") : null, online_meeting: teams } });
      onCreated();
    } catch (x) { onError(String(x)); }
  }

  return (
    <div className="sheet-backdrop" onKeyDown={(k) => { if (k.key === "Escape") onClose(); }}>
      <div className="sheet evsheet">
        <div className="sheet-bar">
          <button onClick={onClose}>Cancel</button>
          <b>{init.teams ? "New Meeting" : "New Event"}</b>
          <button className="primary" onClick={save}>Save</button>
        </div>
        <div className="form">
          <input autoFocus className="big" placeholder="Title" value={title} onChange={(e) => setTitle(e.target.value)} />
          <label className="check"><input type="checkbox" checked={allDay} onChange={(e) => switchAllDay(e.target.checked)} /> All day</label>
          <div className="two">
            <label>Starts<input type={allDay ? "date" : "datetime-local"} value={start} onChange={(e) => setStart(e.target.value)} /></label>
            <label>Ends<input type={allDay ? "date" : "datetime-local"} value={end} onChange={(e) => setEnd(e.target.value)} /></label>
          </div>
          <label>Calendar
            <select value={calId} onChange={(e) => setCalId(e.target.value)}>
              {calendars.map((c) => <option key={c.id} value={c.id}>{c.name}{calendars.some((o) => o.account_email !== c.account_email) ? ` (${c.account_email})` : ""}</option>)}
            </select>
          </label>
          <input placeholder="Location" value={location} onChange={(e) => setLocation(e.target.value)} />
          <input placeholder="Invitees (email addresses)" value={people} onChange={(e) => setPeople(e.target.value)} />
          <label className="check"><input type="checkbox" checked={teams} onChange={(e) => setTeams(e.target.checked)} /> {calendars.find((c) => c.id === calId)?.provider === "google" ? "Google Meet video call" : "Teams meeting"}</label>
          <textarea placeholder="Notes" rows={4} value={notes} onChange={(e) => setNotes(e.target.value)} />
          {err && <div className="error">{err}</div>}
        </div>
      </div>
    </div>
  );
}
