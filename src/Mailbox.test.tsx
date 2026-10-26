// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import Mailbox from "./Mailbox";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ minimize() {}, toggleMaximize() {}, close() {}, startDragging() {} }) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn(), open: vi.fn() }));
// jsdom has no layout, so the real virtualizer would show no rows: list every message.
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getVirtualItems: () => Array.from({ length: count }, (_, index) => ({ index, key: index, start: index * 76, size: 76 })),
    getTotalSize: () => count * 76,
  }),
}));
const mocked = vi.mocked(invoke);
const await_ = <T,>(p: Promise<T>) => p;

beforeAll(() => {
  globalThis.ResizeObserver ??= class { observe() {} unobserve() {} disconnect() {} } as any;
  window.matchMedia ??= ((q: string) => ({ matches: false, media: q, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} })) as any;
});

function backend(accounts: object[], extra: Record<string, (args?: unknown) => unknown> = {}) {
  mocked.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd in extra) return extra[cmd](args);
    switch (cmd) {
      case "list_accounts": return accounts;
      case "get_theme": return "white";
      case "contacts_overview": return {
        directory: { total: 1, rows: [{ id: "d1", source: "directory", account_email: "me@corp.com", name: "Ana Diaz", email: "ana@corp.com", subtitle: "CFO" }] },
        personal: { total: 0, rows: [] },
        accounts: [{ email: "me@corp.com", granted: true, directory: "ok", personal: "ok", directory_count: 1, personal_count: 0, last_sync: 1 }],
      };
      case "contact_details": return [{ id: "d1", source: "directory", account_email: "me@corp.com", folder: null, name: "Ana Diaz", emails: ["ana@corp.com"], phones: [],
        job_title: "CFO", department: null, office: null, company: null, favourite: false }];
      case "list_calendars": return [{ id: "cal1", name: "Calendar", color: "#0a84ff", is_primary: true, can_edit: true, account_email: "me@corp.com", provider: "microsoft" }];
      case "pending_actions": return 0;
      case "sync_now": return { folders: 0, messages_changed: 0, bodies_downloaded: 0, replayed: 0, events: 0 };
      case "search_messages": return [];
      default: return /^(list|pending|search)/.test(String(cmd)) ? [] : null;
    }
  });
}
beforeEach(() => mocked.mockReset());
afterEach(cleanup);

describe("Contacts tab", () => {
  it("opens from the ribbon's View tab next to Mail and Calendar, and from the sidebar", async () => {
    backend([{ email: "me@corp.com", display_name: "Me", provider: "microsoft" }]);
    render(<Mailbox account={{ email: "me@corp.com", display_name: "Me" }} onSignOut={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "View" }));
    const show = screen.getByRole("button", { name: "Mail" }).parentElement!;
    expect(within(show).getAllByRole("button").map((b) => b.textContent)).toEqual(["Mail", "Calendar", "Contacts"]);
    fireEvent.click(within(show).getByRole("button", { name: "Contacts" }));
    expect(await screen.findByRole("group", { name: "Directory" })).toBeTruthy();
    expect(await screen.findByText("Ana Diaz")).toBeTruthy();
    expect(screen.getByRole("region", { name: "Contact details" })).toBeTruthy();
    // Back to mail, then to contacts again through the sidebar.
    fireEvent.click(within(show).getByRole("button", { name: "Mail" }));
    await waitFor(() => expect(screen.queryByRole("group", { name: "Directory" })).toBeNull());
    fireEvent.click(screen.getByText("Contacts", { selector: ".sidebar .name" }));
    expect(await screen.findByRole("group", { name: "Directory" })).toBeTruthy();
  });

  it("shows local contacts (people you've emailed, imported) for a Gmail app-password account, with the Google note", async () => {
    backend([{ email: "me@gmail.com", display_name: "Gm", provider: "imap" }], {
      contacts_overview: () => ({ directory: { total: 0, rows: [] }, personal: { total: 0, rows: [] }, accounts: [],
        emailed: { total: 1, rows: [{ id: "e1", source: "emailed", account_email: "me@gmail.com", name: "Ana Diaz", email: "ana@x.com", subtitle: null, message_count: 3, last_contacted: 1_700_000_000 }] },
        imported: { total: 0, rows: [] } }),
    });
    render(<Mailbox account={{ email: "me@gmail.com", display_name: "Gm" }} onSignOut={() => {}} />);
    await waitFor(() => expect(mocked).toHaveBeenCalledWith("list_accounts"));
    fireEvent.click(screen.getByText("Contacts", { selector: ".sidebar .name" }));
    // The group headings appear as soon as the accounts are known; the rows only once contacts_overview has answered.
    const people = await screen.findByRole("group", { name: "People you've emailed" });
    expect(await within(people).findByText(/3 messages · last/)).toBeTruthy();
    expect(within(people).getByText("Ana Diaz")).toBeTruthy();
    expect(screen.getByRole("group", { name: "Imported" })).toBeTruthy();
    expect(screen.queryByRole("group", { name: "Directory" })).toBeNull();
    expect(screen.getByText(/Google contacts need Google sign-in, coming in a later update\./)).toBeTruthy();
  });

  it("explains that contacts need a Microsoft 365 or Gmail app-password account when only Google sign-in accounts are connected", async () => {
    backend([{ email: "me@gmail.com", display_name: "Gm", provider: "google" }]);
    render(<Mailbox account={{ email: "me@gmail.com", display_name: "Gm" }} onSignOut={() => {}} />);
    await waitFor(() => expect(mocked).toHaveBeenCalledWith("list_accounts"));
    fireEvent.click(screen.getByText("Contacts", { selector: ".sidebar .name" }));
    expect(await screen.findByText("Contacts are available for Microsoft 365 accounts and Gmail (app password) accounts.")).toBeTruthy();
  });

  it("searches mail with a contact and starts a meeting with them from the right-click menu", async () => {
    backend([{ email: "me@corp.com", display_name: "Me", provider: "microsoft" }]);
    render(<Mailbox account={{ email: "me@corp.com", display_name: "Me" }} onSignOut={() => {}} />);
    fireEvent.click(screen.getByText("Contacts", { selector: ".sidebar .name" }));
    fireEvent.contextMenu(await screen.findByText("Ana Diaz"));
    fireEvent.click(await screen.findByRole("menuitem", { name: /Search mail with this person/ }));
    await waitFor(() => expect(mocked).toHaveBeenCalledWith("search_messages", { query: "with:ana@corp.com" }));
    expect((screen.getByPlaceholderText("Search") as HTMLInputElement).value).toBe("with:ana@corp.com");
    expect(screen.getByText("Mail with ana@corp.com")).toBeTruthy();

    fireEvent.click(screen.getByText("Contacts", { selector: ".sidebar .name" }));
    fireEvent.contextMenu(await screen.findByText("Ana Diaz"));
    fireEvent.click(await screen.findByRole("menuitem", { name: /New meeting/ }));
    expect(await screen.findByDisplayValue("ana@corp.com")).toBeTruthy(); // the invitee is filled in
  });
});

describe("Mail list right-click menu", () => {
  const inbox = { id: "f1", remote_id: "r1", name: "Inbox", well_known: "inbox", unread: 1, total: 1, parent_remote_id: null, account_email: "me@corp.com" };
  const msg = { id: "m1", subject: "Hello", sender_name: "Ana", sender_address: "ana@corp.com", snippet: "hi", received_at: 1, is_read: true, is_flagged: false, has_attachments: false, account_email: "me@corp.com" };
  const openMenu = async () => {
    render(<Mailbox account={{ email: "me@corp.com", display_name: "Me" }} onSignOut={() => {}} />);
    fireEvent.contextMenu(await screen.findByText("Hello"));
    return screen.getByRole("menu", { name: "Message actions" });
  };
  const calledWith = (cmd: string) => mocked.mock.calls.filter(([c]) => c === cmd);

  it("never starts the keyboard on Delete, even while Reply is unavailable, and a first Enter can't delete", async () => {
    // get_message never answers: Reply / Reply All / Forward stay disabled, so the first enabled item is Delete.
    backend([{ email: "me@corp.com", display_name: "Me", provider: "microsoft" }], { list_folders: () => [inbox], list_messages: () => [msg], get_message: () => new Promise(() => {}) });
    const menu = await openMenu();
    const focused = document.activeElement as HTMLElement;
    expect(menu.contains(focused)).toBe(true);
    expect(focused.textContent).not.toMatch(/Delete|spam|Block/);
    expect(focused.textContent).toContain("Move to…");
    fireEvent.keyDown(menu, { key: "Enter" });
    expect(calledWith("delete_message")).toHaveLength(0);
    expect(calledWith("mark_spam")).toHaveLength(0);
    expect(screen.getByPlaceholderText("Find a folder")).toBeTruthy(); // Move to… opened the folder picker instead
  });

  it("starts on Reply once the message is open, and Delete still works when chosen on purpose", async () => {
    backend([{ email: "me@corp.com", display_name: "Me", provider: "microsoft" }], { list_folders: () => [inbox], list_messages: () => [msg], get_message: () => ({ ...msg, to_json: null, cc_json: null, body_html: "" }) });
    render(<Mailbox account={{ email: "me@corp.com", display_name: "Me" }} onSignOut={() => {}} />);
    const row = await screen.findByText("Hello", { selector: ".subj" });
    fireEvent.click(row);
    await waitFor(() => expect(screen.getAllByRole("button", { name: /^Reply/ }).some((b) => !(b as HTMLButtonElement).disabled)).toBe(true)); // the message is open
    fireEvent.contextMenu(row);
    const menu = screen.getByRole("menu", { name: "Message actions" });
    expect(document.activeElement?.textContent).toContain("Reply");
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(document.activeElement?.textContent).toContain("Delete");
    fireEvent.keyDown(menu, { key: "Enter" });
    await waitFor(() => expect(calledWith("delete_message")).toHaveLength(1));
  });
});

describe("Sidebar tiles and cards", () => {
  const fold = (id: string, name: string, wk: string | null, parent: string | null = null) =>
    ({ id, remote_id: `r-${id}`, name, well_known: wk, unread: 0, total: 0, parent_remote_id: parent ? `r-${parent}` : null, account_email: "me@corp.com" });
  const FOLDERS = [fold("in", "Posteingang", "inbox"), fold("sent", "Gesendet", "sentitems"), fold("proj", "Deleted Items", null), fold("sub", "2026", null, "proj"),
    fold("sync", "Synchronisierungsprobleme", "syncissues"), fold("conf", "Konflikte", null, "sync")];

  it("gives every folder a flat tile by role (never by name), indents subfolders, and keeps tiles decorative", async () => {
    backend([{ email: "me@corp.com", display_name: "Me", provider: "microsoft" }], { list_folders: () => FOLDERS });
    render(<Mailbox account={{ email: "me@corp.com", display_name: "Me" }} onSignOut={() => {}} />);
    const row = (name: string) => (await_(screen.findByText(name, { selector: ".sidebar .folder .name" })));
    const inbox = (await row("Posteingang")).closest(".folder")!;
    expect(inbox.querySelector(".tile")!.getAttribute("data-role")).toBe("inbox");
    expect((inbox.querySelector(".tile") as HTMLElement).style.background).toBe("rgb(47, 111, 214)");
    expect(inbox.querySelector(".tile")!.getAttribute("aria-hidden")).toBe("true");
    expect(inbox.querySelector("svg path")!.getAttribute("d")).toContain("M3 13l3-8h12l3 8v6H3z");
    expect((await row("Gesendet")).closest(".folder")!.querySelector(".tile")!.getAttribute("data-role")).toBe("sent");
    const custom = (await row("Deleted Items")).closest(".folder") as HTMLElement; // a user folder named like a system one
    expect(custom.querySelector(".tile")!.getAttribute("data-role")).toBe("folder");
    const sub = (await row("2026")).closest(".folder") as HTMLElement;
    expect(parseInt(sub.style.paddingLeft) - parseInt(custom.style.paddingLeft)).toBe(22);
    expect((await row("Konflikte")).closest(".folder")!.querySelector(".tile")!.getAttribute("data-role")).toBe("syncissues");
  });

  it("shows Master Calendar and Contacts as cards with subtitles, and the active one is marked", async () => {
    backend([{ email: "me@corp.com", display_name: "Me", provider: "microsoft" }], { list_folders: () => FOLDERS });
    render(<Mailbox account={{ email: "me@corp.com", display_name: "Me" }} onSignOut={() => {}} />);
    const cal = screen.getByRole("button", { name: /Master Calendar/ });
    const people = screen.getByRole("button", { name: /Contacts/ });
    expect(cal.textContent).toContain("All calendars");
    expect(people.textContent).toContain("People and directory");
    expect(cal.querySelector(".tile")!.getAttribute("data-role")).toBe("calendar");
    expect(people.querySelector(".tile")!.getAttribute("data-role")).toBe("contacts");
    expect(cal.getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(cal);
    expect(cal.getAttribute("aria-pressed")).toBe("true");
    expect(cal.className).toContain("active");
    fireEvent.keyDown(people, { key: "Enter" });
    expect(people.getAttribute("aria-pressed")).toBe("true");
    expect(cal.getAttribute("aria-pressed")).toBe("false");
  });
});

describe("Message bodies", () => {
  const inbox = { id: "f1", remote_id: "r1", name: "Inbox", well_known: "inbox", unread: 0, total: 1, parent_remote_id: null, account_email: "me@corp.com" };
  const msg = { id: "m1", subject: "Newsletter", sender_name: "Shop", sender_address: "news@shop.example", snippet: "", received_at: 1, is_read: true, is_flagged: false, has_attachments: false, account_email: "me@corp.com" };
  const body = '<p>Sale!</p><img src="https://track.example/p.gif" width="1" height="1"><img src="data:image/png;base64,AAAA" alt="logo"><a href="javascript:alert(1)">x</a><script>alert(1)</script>';
  const frame = () => document.querySelector(".reader iframe") as HTMLIFrameElement;

  async function openMessage(trusted: { value: boolean }) {
    const calls: [string, unknown][] = [];
    backend([{ email: "me@corp.com", display_name: "Me", provider: "microsoft" }], {
      list_folders: () => [inbox], list_messages: () => [msg],
      get_message: () => ({ ...msg, to_json: null, cc_json: null, body_html: body }),
      image_sender_allowed: (a) => { calls.push(["image_sender_allowed", a]); return trusted.value; },
      set_image_sender: (a) => { calls.push(["set_image_sender", a]); trusted.value = (a as { allowed: boolean }).allowed; return null; },
    });
    render(<Mailbox account={{ email: "me@corp.com", display_name: "Me" }} onSignOut={() => {}} />);
    fireEvent.click(await screen.findByText("Newsletter", { selector: ".subj" }));
    await waitFor(() => expect(frame()).toBeTruthy());
    return calls;
  }

  it("shows the body in a frame without scripts or the app's origin, and blocks remote images until asked", async () => {
    await openMessage({ value: false });
    const f = frame();
    expect(f.getAttribute("sandbox")).toBe("allow-popups");
    const doc = f.getAttribute("srcdoc")!;
    expect(doc).not.toMatch(/track\.example|<script|javascript:/);
    expect(doc).toContain("data:image/png;base64,AAAA"); // inline pictures still show
    expect(screen.getByText(/Images from the internet were blocked/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Load images" }));
    await waitFor(() => expect(frame().getAttribute("srcdoc")).toContain("https://track.example/p.gif"));
    expect(frame().getAttribute("srcdoc")).not.toMatch(/<script|javascript:/);
    expect(screen.queryByText(/Images from the internet were blocked/)).toBeNull();
  });

  it("can always load images from one sender, and stop again", async () => {
    const trusted = { value: false };
    const calls = await openMessage(trusted);
    expect(calls).toContainEqual(["image_sender_allowed", { address: "news@shop.example" }]);
    fireEvent.click(screen.getByRole("button", { name: "Always load from news@shop.example" }));
    await waitFor(() => expect(frame().getAttribute("srcdoc")).toContain("https://track.example/p.gif"));
    expect(calls).toContainEqual(["set_image_sender", { address: "news@shop.example", allowed: true }]);
    fireEvent.click(await screen.findByRole("button", { name: "Stop loading them" }));
    await waitFor(() => expect(frame().getAttribute("srcdoc")).not.toContain("track.example"));
    expect(calls).toContainEqual(["set_image_sender", { address: "news@shop.example", allowed: false }]);
  });

  it("loads a trusted sender's images straight away", async () => {
    await openMessage({ value: true });
    await waitFor(() => expect(frame().getAttribute("srcdoc")).toContain("https://track.example/p.gif"));
    expect(screen.getByText(/are always loaded/)).toBeTruthy();
  });
});
