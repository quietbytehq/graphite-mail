// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import ContactsView, { ContactDetail, ContactRow, ContactStatus, GOOGLE_CONTACTS_NOTE, Overview, contactedLine, importLine, statusNote } from "./ContactsView";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: vi.fn(async () => "/home/me/Ana Diaz.vcf"), open: vi.fn(async () => "/home/me/contacts.csv") }));
const google = { email: "me@gmail.com", display_name: "Gm", provider: "google" };
const mocked = vi.mocked(invoke);

const ms = { email: "me@corp.com", display_name: "Me", provider: "microsoft" };
const ms2 = { email: "you@other.com", display_name: "You", provider: "microsoft" };
const gmail = { email: "me@gmail.com", display_name: "Gm", provider: "imap" };

const row = (id: string, name: string, source: ContactRow["source"], email: string, subtitle: string | null = null): ContactRow =>
  ({ id, source, account_email: "me@corp.com", name, email, subtitle });
const ROWS = [
  row("d1", "Ana Diaz", "directory", "ana@corp.com", "CFO"),
  row("d2", "Ben Ode", "directory", "ben@corp.com"),
  row("p1", "Anna Bell", "personal", "anna@home.com"),
];
const ok: ContactStatus = { email: "me@corp.com", granted: true, directory: "ok", personal: "ok", directory_count: 2, personal_count: 1, last_sync: 1 };
const DETAIL: ContactDetail = {
  id: "d1", source: "directory", account_email: "me@corp.com", folder: null, name: "Ana Diaz", emails: ["ana@corp.com", "a.diaz@corp.com"],
  phones: [{ kind: "work", number: "+1 555 0100" }], job_title: "CFO", department: "Finance", office: "HQ 4", company: "Contoso",
};

/** A pretend backend: filters ROWS like the real search does, and records calls. */
function backend(status: ContactStatus = ok) {
  mocked.mockImplementation(async (cmd: string, args?: any) => {
    if (cmd === "contacts_overview") {
      const words = String(args.query).toLowerCase().split(/\s+/).filter(Boolean);
      const hit = (r: ContactRow) => words.every((w) => `${r.name} ${r.email} ${r.subtitle ?? ""}`.toLowerCase().includes(w));
      const of = (s: string) => { const rows = ROWS.filter((r) => r.source === s && hit(r)); return { total: rows.length, rows }; };
      return { directory: of("directory"), personal: of("personal"), accounts: [status] } satisfies Overview;
    }
    if (cmd === "contact_detail") return DETAIL;
    if (cmd === "refresh_contacts" || cmd === "grant_contacts") return [ok];
    return null;
  });
}
const calls = (cmd: string) => mocked.mock.calls.filter(([c]) => c === cmd);

beforeEach(() => { mocked.mockReset(); });
afterEach(cleanup);

describe("Contacts view", () => {
  it("shows the directory and my contacts as groups with counts, and folds them", async () => {
    backend();
    render(<ContactsView accounts={[ms, google]} onNewEmail={() => {}} onToast={() => {}} />); // a Google sign-in account has no contacts yet
    const dir = await screen.findByRole("group", { name: "Directory" });
    await waitFor(() => expect(within(dir).getByText("Ana Diaz")).toBeTruthy());
    expect(within(dir).getByRole("button", { name: /Directory\s*2/ })).toBeTruthy();
    expect(within(dir).getByText("CFO")).toBeTruthy();
    const mine = screen.getByRole("group", { name: "My contacts" });
    expect(within(mine).getByRole("button", { name: /My contacts\s*1/ })).toBeTruthy();
    expect(within(mine).getByText("Anna Bell")).toBeTruthy();
    // Folding a group hides its people.
    const head = within(dir).getByRole("button", { name: /Directory/ });
    fireEvent.click(head);
    expect(head.getAttribute("aria-expanded")).toBe("false");
    expect(within(dir).queryByText("Ana Diaz")).toBeNull();
    fireEvent.click(head);
    expect(within(dir).getByText("Ana Diaz")).toBeTruthy();
    expect(screen.queryByRole("combobox", { name: "Account" })).toBeNull(); // one Microsoft account: no account filter
  });

  it("searches both groups with one box", async () => {
    backend();
    render(<ContactsView accounts={[ms]} onNewEmail={() => {}} onToast={() => {}} />);
    await screen.findByText("Ben Ode");
    fireEvent.change(screen.getByRole("searchbox", { name: "Search contacts" }), { target: { value: "an" } });
    await waitFor(() => expect(screen.queryByText("Ben Ode")).toBeNull());
    expect(screen.getByText("Ana Diaz")).toBeTruthy();
    expect(screen.getByText("Anna Bell")).toBeTruthy();
    expect(calls("contacts_overview").slice(-1)[0][1]).toEqual({ query: "an", account: null });
    fireEvent.change(screen.getByRole("searchbox", { name: "Search contacts" }), { target: { value: "zzz" } });
    await waitFor(() => expect(screen.getAllByText("No matches")).toHaveLength(2));
  });

  it("shows the selected person, writes to them and copies their address", async () => {
    backend();
    const onNewEmail = vi.fn();
    const onToast = vi.fn();
    const writeText = vi.fn(() => Promise.resolve());
    Object.assign(navigator, { clipboard: { writeText } });
    render(<ContactsView accounts={[ms]} onNewEmail={onNewEmail} onToast={onToast} />);
    fireEvent.click(await screen.findByText("Ana Diaz"));
    const pane = screen.getByRole("region", { name: "Contact details" });
    await waitFor(() => expect(within(pane).getByRole("heading", { name: "Ana Diaz" })).toBeTruthy());
    for (const t of ["ana@corp.com", "a.diaz@corp.com", "+1 555 0100", "HQ 4", "Contoso", "Finance"]) expect(within(pane).getAllByText(new RegExp(t.replace(/[+.]/g, "\\$&"))).length).toBeGreaterThan(0);
    fireEvent.click(within(pane).getByRole("button", { name: /New email/ }));
    expect(onNewEmail).toHaveBeenCalledWith(["ana@corp.com"], "me@corp.com");
    await act(async () => { fireEvent.click(within(pane).getByRole("button", { name: /Copy email/ })); });
    expect(writeText).toHaveBeenCalledWith("ana@corp.com");
    expect(onToast).toHaveBeenCalledWith("Email address copied");
  });

  it("explains missing permission and offers Grant access", async () => {
    backend({ ...ok, granted: false, directory: "needs_consent", personal: "needs_consent", directory_count: 0, personal_count: 0 });
    render(<ContactsView accounts={[ms]} onNewEmail={() => {}} onToast={() => {}} />);
    expect(await screen.findByText(/needs your permission to read contacts/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Grant access" }));
    await waitFor(() => expect(calls("grant_contacts")[0][1]).toEqual({ email: "me@corp.com" }));
  });

  it("refreshes on request and filters by account when several are connected", async () => {
    backend();
    render(<ContactsView accounts={[ms, ms2]} onNewEmail={() => {}} onToast={() => {}} />);
    await screen.findByText("Ana Diaz");
    fireEvent.click(screen.getByRole("button", { name: "Refresh contacts" }));
    await waitFor(() => expect(calls("refresh_contacts").length).toBe(1));
    fireEvent.change(screen.getByRole("combobox", { name: "Account" }), { target: { value: "you@other.com" } });
    await waitFor(() => expect(calls("contacts_overview").slice(-1)[0][1]).toEqual({ query: "", account: "you@other.com" }));
  });

  it("downloads at once the first time", async () => {
    backend({ ...ok, directory: "never", personal: "never" });
    render(<ContactsView accounts={[ms]} onNewEmail={() => {}} onToast={() => {}} />);
    await waitFor(() => expect(calls("refresh_contacts").length).toBe(1));
  });

  it("says contacts need a Microsoft 365 or Gmail app-password account (Google sign-in accounts have none yet)", () => {
    render(<ContactsView accounts={[google]} onNewEmail={() => {}} onToast={() => {}} />);
    expect(screen.getByText("Contacts are available for Microsoft 365 accounts and Gmail (app password) accounts.")).toBeTruthy();
    expect(mocked).not.toHaveBeenCalled();
  });

  it("words each problem", () => {
    expect(statusNote(ok)).toBeNull();
    expect(statusNote({ ...ok, directory: "blocked" })?.text).toMatch(/doesn't allow Graphite Mail to read the directory/);
    expect(statusNote({ ...ok, directory: "blocked" })?.grant).toBe(true);
    expect(statusNote({ ...ok, personal: "removed", directory: "removed" })?.grant).toBe(false);
    expect(statusNote({ ...ok, personal: "error" })?.text).toMatch(/offline/);
  });
});

// ---------------------------------------------------------------- right-click menu, multi-select, favourites

const person = (id: string, name: string, emails: string[], phones: { kind: string; number: string }[] = [], favourite = false): ContactDetail =>
  ({ id, source: id.startsWith("p") ? "personal" : "directory", account_email: "me@corp.com", folder: null, name, emails, phones, job_title: null, department: null, office: null, company: null, favourite });
const PEOPLE: Record<string, ContactDetail> = {
  d1: person("d1", "Ana Diaz", ["ana@corp.com", "a.diaz@corp.com"], [{ kind: "work", number: "+1 555 0100" }]),
  d2: person("d2", "Ben Ode", ["ben@corp.com"]),
  d3: person("d3", "Room 4.01", []),
  p1: person("p1", "Anna Bell", ["anna@home.com"], [], true),
};
const LIST: ContactRow[] = [
  row("d1", "Ana Diaz", "directory", "ana@corp.com"), row("d2", "Ben Ode", "directory", "ben@corp.com"),
  { ...row("d3", "Room 4.01", "directory", ""), email: null }, { ...row("p1", "Anna Bell", "personal", "anna@home.com"), favourite: true },
];
function people(favourites = true) {
  mocked.mockImplementation(async (cmd: string, args?: any) => {
    switch (cmd) {
      case "contacts_overview": {
        const of = (s: string) => { const rows = LIST.filter((r) => r.source === s); return { total: rows.length, rows }; };
        const fav = favourites ? LIST.filter((r) => r.favourite) : [];
        return { favourites: { total: fav.length, rows: fav }, directory: of("directory"), personal: of("personal"), accounts: [ok] };
      }
      case "contact_detail": return PEOPLE[args.id];
      case "contact_details": return args.ids.map((id: string) => PEOPLE[id]);
      case "export_vcards": return args.ids.length;
      default: return null;
    }
  });
}
const handlers = () => ({ onNewEmail: vi.fn(), onNewMeeting: vi.fn(), onSearchMail: vi.fn(), onBlock: vi.fn(), onCreateRule: vi.fn(), onToast: vi.fn() });
const rowOf = (name: string) => screen.getAllByRole("option").find((o) => o.textContent?.includes(name) && o.closest("[aria-label='Directory list'], [aria-label='My contacts list']"))!;
const menuLabels = () => within(screen.getByRole("menu", { name: "Contact actions" })).getAllByRole("menuitem").map((m) => m.textContent!.replace("›", "").trim());

describe("Contacts right-click menu", () => {
  it("selects the contact first and offers its actions, with a submenu for several addresses", async () => {
    people();
    const h = handlers();
    render(<ContactsView accounts={[ms]} {...h} />);
    await screen.findByText("Ben Ode");
    fireEvent.contextMenu(rowOf("Ana Diaz"), { clientX: 50, clientY: 60 });
    await screen.findByRole("menu", { name: "Contact actions" });
    expect(rowOf("Ana Diaz").getAttribute("aria-selected")).toBe("true");
    expect(menuLabels()).toEqual(["New email", "New meeting", "Search mail with this person", "Copy email address", "Copy name and address",
      "Copy phone number", "Add to favourites", "Export as vCard…", "Block sender", "Create rule for this sender…"]);
    const newEmail = screen.getByRole("menuitem", { name: /New email/ });
    expect(newEmail.getAttribute("aria-haspopup")).toBe("menu");
    fireEvent.mouseEnter(newEmail);
    fireEvent.click(within(screen.getByRole("menu", { name: "New email" })).getByRole("menuitem", { name: "a.diaz@corp.com" }));
    expect(h.onNewEmail).toHaveBeenCalledWith(["a.diaz@corp.com"], "me@corp.com");
    expect(screen.queryByRole("menu", { name: "Contact actions" })).toBeNull();
  });

  it("hides email items for a contact without an address", async () => {
    people();
    render(<ContactsView accounts={[ms]} {...handlers()} />);
    await screen.findByText("Room 4.01");
    fireEvent.contextMenu(rowOf("Room 4.01"));
    await screen.findByRole("menu", { name: "Contact actions" });
    expect(menuLabels()).toEqual(["Add to favourites", "Export as vCard…"]);
  });

  it("searches mail, blocks and makes rules through the app, and exports a vCard", async () => {
    people();
    const h = handlers();
    render(<ContactsView accounts={[ms]} {...h} />);
    await screen.findByText("Ben Ode");
    const run = async (name: string, item: RegExp) => {
      fireEvent.contextMenu(rowOf(name));
      fireEvent.click(await screen.findByRole("menuitem", { name: item }));
    };
    await run("Ana Diaz", /Search mail with this person/);
    expect(h.onSearchMail).toHaveBeenCalledWith(["ana@corp.com", "a.diaz@corp.com"]);
    await run("Ben Ode", /Block sender/);
    expect(h.onBlock).toHaveBeenCalledWith("me@corp.com", "ben@corp.com");
    await run("Ben Ode", /Create rule for this sender/);
    expect(h.onCreateRule).toHaveBeenCalledWith("me@corp.com", "ben@corp.com");
    await run("Ben Ode", /New meeting/);
    expect(h.onNewMeeting).toHaveBeenCalledWith(["ben@corp.com"], "me@corp.com");
    await run("Ben Ode", /Export as vCard/);
    await waitFor(() => expect(calls("export_vcards")[0][1]).toEqual({ ids: ["d2"], path: "/home/me/Ana Diaz.vcf" }));
    await run("Ben Ode", /Add to favourites/);
    await waitFor(() => expect(calls("set_contact_favourite")[0][1]).toEqual({ id: "d2", on: true }));
  });

  it("selects with Ctrl+click, Shift+click and Ctrl+A, and offers group actions", async () => {
    people();
    const h = handlers();
    render(<ContactsView accounts={[ms]} {...h} />);
    await screen.findByText("Ben Ode");
    fireEvent.click(rowOf("Ana Diaz"));
    fireEvent.click(rowOf("Anna Bell"), { ctrlKey: true });
    expect(screen.getByText("2 contacts selected")).toBeTruthy();
    fireEvent.contextMenu(rowOf("Anna Bell"));
    await screen.findByRole("menu", { name: "Contact actions" });
    expect(menuLabels()).toEqual(["New email to all (2)", "New meeting with all (2)", "Copy all addresses", "Export selected as vCard (2)…"]);
    fireEvent.click(screen.getByRole("menuitem", { name: /New email to all/ }));
    expect(h.onNewEmail).toHaveBeenCalledWith(["anna@home.com", "ana@corp.com"], "me@corp.com");
    // Shift+click: a range in list order (favourites first, then the directory).
    fireEvent.click(rowOf("Ben Ode"));
    fireEvent.click(rowOf("Room 4.01"), { shiftKey: true });
    expect(screen.getByText("2 contacts selected")).toBeTruthy();
    // Ctrl+A in the list selects everyone shown (a favourite listed twice counts once); Esc clears.
    const list = screen.getByLabelText("Contact list");
    fireEvent.keyDown(list, { key: "a", ctrlKey: true });
    expect(screen.getByText("4 contacts selected")).toBeTruthy();
    fireEvent.keyDown(list, { key: "Escape" });
    expect(screen.queryByText(/contacts selected/)).toBeNull();
    // The hover checkbox adds to the selection like Ctrl+click.
    fireEvent.click(rowOf("Ana Diaz"));
    fireEvent.click(screen.getByRole("checkbox", { name: "Select Ben Ode" }));
    expect(screen.getByText("2 contacts selected")).toBeTruthy();
  });

  it("opens with the keyboard (Shift+F10) and closes with Esc", async () => {
    people();
    render(<ContactsView accounts={[ms]} {...handlers()} />);
    await screen.findByText("Ben Ode");
    fireEvent.click(rowOf("Ben Ode"));
    fireEvent.keyDown(screen.getByLabelText("Contact list"), { key: "F10", shiftKey: true });
    const menu = await screen.findByRole("menu", { name: "Contact actions" });
    expect(document.activeElement?.textContent).toContain("New email");
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(document.activeElement?.textContent).toContain("New meeting");
    fireEvent.keyDown(menu, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("shows favourites in their own group at the top, and only when there are some", async () => {
    people();
    const { unmount } = render(<ContactsView accounts={[ms]} {...handlers()} />);
    await screen.findByText("Ben Ode");
    const groups = screen.getAllByRole("group");
    expect(groups.map((g) => g.getAttribute("aria-label"))).toEqual(["Favourites", "Directory", "My contacts"]);
    const fav = screen.getByRole("group", { name: "Favourites" });
    expect(within(fav).getByRole("button", { name: /Favourites\s*1/ })).toBeTruthy();
    expect(within(fav).getByText("Anna Bell")).toBeTruthy();
    fireEvent.contextMenu(within(fav).getByText("Anna Bell"));
    expect(await screen.findByRole("menuitem", { name: /Remove from favourites/ })).toBeTruthy();
    unmount();
    people(false);
    render(<ContactsView accounts={[ms]} {...handlers()} />);
    await screen.findByText("Ben Ode");
    expect(screen.queryByRole("group", { name: "Favourites" })).toBeNull();
  });
});

describe("Gmail (app password) contacts", () => {
  const emailedRow = (id: string, name: string, email: string, count: number, last: number): ContactRow =>
    ({ id, source: "emailed", account_email: "me@gmail.com", name, email, subtitle: null, message_count: count, last_contacted: last });
  const importedRow = (id: string, name: string, email: string): ContactRow => ({ id, source: "imported", account_email: "me@gmail.com", name, email, subtitle: "Contoso" });
  function local(extra: Record<string, (args?: any) => unknown> = {}) {
    mocked.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd in extra) return extra[cmd](args);
      switch (cmd) {
        case "contacts_overview": return {
          favourites: { total: 0, rows: [] }, directory: { total: 2, rows: ROWS.filter((r) => r.source === "directory") }, personal: { total: 0, rows: [] },
          emailed: { total: 2, rows: [emailedRow("e1", "Ben Ode", "ben@x.com", 12, 1_750_000_000), emailedRow("e2", "ana@x.com", "ana@x.com", 1, 1_700_000_000)] },
          imported: { total: 1, rows: [importedRow("i1", "Cara Lim", "cara@x.com")] },
          accounts: [ok],
        } satisfies Overview;
        case "contact_detail": return { id: "e1", source: "emailed", account_email: "me@gmail.com", folder: null, name: "Ben Ode", emails: ["ben@x.com"], phones: [],
          job_title: null, department: null, office: null, company: null, message_count: 12, last_contacted: 1_750_000_000 } satisfies ContactDetail;
        case "import_contacts": return { added: 3, updated: 1, skipped: 0 };
        default: return null;
      }
    });
  }

  it("shows people you've emailed (with counts) and imported contacts, the Google note and Import, but no directory", async () => {
    local();
    render(<ContactsView accounts={[gmail]} onNewEmail={() => {}} onToast={() => {}} />);
    const people = await screen.findByRole("group", { name: "People you've emailed" });
    // The heading shows before the overview answers (count 0, no rows): wait for a row, not just the group.
    expect(await within(people).findByText("Ben Ode")).toBeTruthy();
    expect(within(people).getByRole("button", { name: /People you've emailed\s*2/ })).toBeTruthy();
    expect(within(people).getByText(/ben@x\.com · 12 messages · last/)).toBeTruthy();
    expect(within(people).getByText(/1 message · last/)).toBeTruthy();
    const imported = screen.getByRole("group", { name: "Imported" });
    expect(await within(imported).findByText("Cara Lim")).toBeTruthy();
    expect(screen.queryByRole("group", { name: "Directory" })).toBeNull();
    expect(screen.queryByRole("group", { name: "My contacts" })).toBeNull();
    expect(screen.getByRole("note").textContent).toContain(GOOGLE_CONTACTS_NOTE);
    expect(screen.getByRole("button", { name: "Import contacts…" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Grant access" })).toBeNull();
    // The detail pane shows the counts and the source.
    fireEvent.click(within(people).getByText("Ben Ode"));
    const pane = screen.getByRole("region", { name: "Contact details" });
    await waitFor(() => expect(within(pane).getByText("12")).toBeTruthy());
    expect(within(pane).getByText("People you've emailed")).toBeTruthy();
  });

  it("imports a file into the Gmail account and says what happened", async () => {
    local();
    const onToast = vi.fn();
    render(<ContactsView accounts={[gmail]} onNewEmail={() => {}} onToast={onToast} />);
    fireEvent.click(await screen.findByRole("button", { name: "Import contacts…" }));
    await waitFor(() => expect(mocked).toHaveBeenCalledWith("import_contacts", { account: "me@gmail.com", path: "/home/me/contacts.csv" }));
    await waitFor(() => expect(onToast).toHaveBeenCalledWith("4 contacts imported (1 updated)"));
  });

  it("with both kinds of account shows every group, and the account filter narrows to one kind", async () => {
    local();
    render(<ContactsView accounts={[ms, gmail]} onNewEmail={() => {}} onToast={() => {}} />);
    expect(await screen.findByRole("group", { name: "Directory" })).toBeTruthy();
    expect(await screen.findByText("Ana Diaz")).toBeTruthy(); // data has arrived
    expect(screen.getByRole("group", { name: "People you've emailed" })).toBeTruthy();
    const select = screen.getByRole("combobox", { name: "Account" }) as HTMLSelectElement;
    expect(select.options[0].textContent).toBe("All accounts");
    fireEvent.change(select, { target: { value: "me@corp.com" } });
    await waitFor(() => expect(screen.queryByRole("group", { name: "People you've emailed" })).toBeNull());
    expect(screen.queryByRole("note")).toBeNull(); // the Google note only goes with Gmail accounts
    fireEvent.change(select, { target: { value: "me@gmail.com" } });
    await waitFor(() => expect(screen.queryByRole("group", { name: "Directory" })).toBeNull());
    expect(screen.getByRole("group", { name: "Imported" })).toBeTruthy();
  });

  it("words the counts and the import result", () => {
    expect(contactedLine(1, null)).toBe("1 message");
    expect(contactedLine(0, undefined)).toBe("0 messages");
    expect(contactedLine(5, 1_700_000_000)).toMatch(/^5 messages · last .*2023$/);
    expect(importLine({ added: 1, updated: 0, skipped: 0 })).toBe("1 contact imported");
    expect(importLine({ added: 2, updated: 1, skipped: 3 })).toBe("3 contacts imported (1 updated, 3 skipped)");
  });
});

describe("Personal Microsoft accounts", () => {
  it("hides the Directory group and shows no consent prompt for the directory", async () => {
    const personal: ContactStatus = { email: "ann@outlook.com", granted: true, personal_account: true, directory: "n/a", personal: "ok", directory_count: 0, personal_count: 1, last_sync: 1 };
    mocked.mockImplementation(async (cmd: string) => {
      if (cmd === "contacts_overview") return { favourites: { total: 0, rows: [] }, directory: { total: 0, rows: [] }, personal: { total: 1, rows: [{ ...row("p1", "Anna Bell", "personal", "anna@home.com"), account_email: "ann@outlook.com" }] }, accounts: [personal] } satisfies Overview;
      return null;
    });
    render(<ContactsView accounts={[{ email: "ann@outlook.com", display_name: "Ann", provider: "microsoft", personal: true }]} onNewEmail={() => {}} onToast={() => {}} />);
    expect(await screen.findByRole("group", { name: "My contacts" })).toBeTruthy();
    await screen.findByText("Anna Bell");
    expect(screen.queryByRole("group", { name: "Directory" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Grant access" })).toBeNull();
    expect(statusNote(personal)).toBeNull();
  });
});
