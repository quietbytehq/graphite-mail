// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import Compose from "./Compose";
import { lastToken, replaceLastToken } from "./recipients";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
const mocked = vi.mocked(invoke);

const people = [{ name: "Anna Bell", email: "anna@home.com" }, { name: "Ana Diaz", email: "ana@corp.com" }];

beforeEach(() => {
  mocked.mockReset();
  mocked.mockImplementation(async (cmd: string, args?: any) => {
    if (cmd === "suggest_recipients") return args.account === "me@corp.com" && args.query.length >= 2 ? people.filter((p) => `${p.name} ${p.email}`.toLowerCase().includes(args.query)) : [];
    if (cmd === "get_signature") return "";
    return null;
  });
});
afterEach(cleanup);

const open = () => render(<Compose init={{ mode: "new" }} accounts={[{ email: "me@corp.com", display_name: "Me" }]} onClose={() => {}} onSent={() => {}} />);

describe("To / Cc suggestions", () => {
  it("suggests contacts while typing and completes with the keyboard", async () => {
    open();
    const to = screen.getByRole("combobox", { name: "To" });
    fireEvent.change(to, { target: { value: "an" } });
    const list = await screen.findByRole("listbox", { name: "To suggestions" });
    expect(list.textContent).toContain("Anna Bell");
    expect(to.getAttribute("aria-expanded")).toBe("true");
    expect(mocked).toHaveBeenCalledWith("suggest_recipients", { query: "an", account: "me@corp.com" });
    fireEvent.keyDown(to, { key: "ArrowDown" });
    expect(screen.getAllByRole("option")[1].getAttribute("aria-selected")).toBe("true");
    fireEvent.keyDown(to, { key: "Enter" });
    expect((to as HTMLInputElement).value).toBe("ana@corp.com, ");
    await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
  });

  it("works in Cc too, by clicking a suggestion, after addresses already typed", async () => {
    open();
    const cc = screen.getByRole("combobox", { name: "Cc" });
    fireEvent.change(cc, { target: { value: "bob@x.com, ann" } });
    const option = await screen.findByRole("option", { name: /Anna Bell/ });
    fireEvent.mouseDown(option);
    expect((cc as HTMLInputElement).value).toBe("bob@x.com, anna@home.com, ");
  });

  it("stays quiet for one letter and when nothing matches", async () => {
    open();
    const to = screen.getByRole("combobox", { name: "To" });
    fireEvent.change(to, { target: { value: "a" } });
    fireEvent.change(to, { target: { value: "zz" } });
    await waitFor(() => expect(mocked).toHaveBeenCalledWith("suggest_recipients", { query: "zz", account: "me@corp.com" }));
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(mocked.mock.calls.some(([c, a]: any) => c === "suggest_recipients" && a.query === "a")).toBe(false);
  });
});

describe("To / Cc suggestions (quiet cases)", () => {
  it("doesn't pop up for addresses that were already there when the message opened", async () => {
    render(<Compose init={{ mode: "new", to: ["ana@corp.com"] }} accounts={[{ email: "me@corp.com", display_name: "Me" }]} onClose={() => {}} onSent={() => {}} />);
    await new Promise((r) => setTimeout(r, 200));
    expect(mocked.mock.calls.some(([c]) => c === "suggest_recipients")).toBe(false);
    expect(screen.queryByRole("listbox")).toBeNull();
  });

  it("closes when the field loses focus", async () => {
    open();
    const to = screen.getByRole("combobox", { name: "To" });
    fireEvent.change(to, { target: { value: "an" } });
    await screen.findByRole("listbox", { name: "To suggestions" });
    fireEvent.blur(to);
    expect(screen.queryByRole("listbox")).toBeNull();
  });
});

describe("recipient tokens", () => {
  it("finds and replaces the address being typed", () => {
    expect(lastToken("a@x.com, bo")).toBe("bo");
    expect(lastToken("a@x.com; bo ")).toBe("bo");
    expect(lastToken("bo")).toBe("bo");
    expect(replaceLastToken("bo", "bob@x.com")).toBe("bob@x.com, ");
    expect(replaceLastToken("a@x.com, bo", "bob@x.com")).toBe("a@x.com, bob@x.com, ");
    expect(replaceLastToken("a@x.com;bo", "bob@x.com")).toBe("a@x.com; bob@x.com, ");
  });
});

describe("Bcc", () => {
  const sent = () => mocked.mock.calls.filter(([c]) => c === "send_mail").map(([, a]: any) => a.draft);

  it("is hidden until the Bcc link opens it, then suggests contacts like To and Cc", async () => {
    open();
    expect(screen.queryByRole("combobox", { name: "Bcc" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Bcc" }));
    const bcc = screen.getByRole("combobox", { name: "Bcc" });
    expect(screen.queryByRole("button", { name: "Bcc" })).toBeNull(); // the link goes away once the row is open
    fireEvent.change(bcc, { target: { value: "an" } });
    await screen.findByRole("listbox", { name: "Bcc suggestions" });
    expect(mocked).toHaveBeenCalledWith("suggest_recipients", { query: "an", account: "me@corp.com" });
    fireEvent.keyDown(bcc, { key: "Enter" });
    expect((bcc as HTMLInputElement).value).toBe("anna@home.com, ");
  });

  it("sends blind copies separately from To and Cc, so they are never in the visible recipients", async () => {
    mocked.mockImplementation(async (cmd: string) => (cmd === "send_mail" ? true : cmd === "get_signature" ? "" : null));
    open();
    fireEvent.change(screen.getByRole("combobox", { name: "To" }), { target: { value: "a@x.com" } });
    fireEvent.change(screen.getByRole("combobox", { name: "Cc" }), { target: { value: "c@x.com" } });
    fireEvent.click(screen.getByRole("button", { name: "Bcc" }));
    fireEvent.change(screen.getByRole("combobox", { name: "Bcc" }), { target: { value: "hidden@x.com; other@x.com" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(sent()).toHaveLength(1));
    const d = sent()[0];
    expect(d.to).toEqual(["a@x.com"]);
    expect(d.cc).toEqual(["c@x.com"]);
    expect(d.bcc).toEqual(["hidden@x.com", "other@x.com"]);
    expect([...d.to, ...d.cc]).not.toContain("hidden@x.com");
  });

  it("a message with only blind copies can be sent; a bad Bcc address is caught", async () => {
    mocked.mockImplementation(async (cmd: string) => (cmd === "send_mail" ? true : cmd === "get_signature" ? "" : null));
    open();
    fireEvent.click(screen.getByRole("button", { name: "Bcc" }));
    fireEvent.change(screen.getByRole("combobox", { name: "Bcc" }), { target: { value: "not-an-address" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(await screen.findByText(/Check these addresses: not-an-address/)).toBeTruthy();
    expect(sent()).toHaveLength(0);
    fireEvent.change(screen.getByRole("combobox", { name: "Bcc" }), { target: { value: "hidden@x.com" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(sent()).toHaveLength(1));
    expect(sent()[0]).toMatchObject({ to: [], cc: [], bcc: ["hidden@x.com"] });
  });

  it("is saved with the draft and shown again when the draft has blind copies", async () => {
    mocked.mockImplementation(async (cmd: string) => (cmd === "save_draft" ? "d1" : cmd === "get_signature" ? "" : null));
    const onClose = vi.fn();
    render(<Compose init={{ mode: "new", bcc: ["hidden@x.com"] }} accounts={[{ email: "me@corp.com", display_name: "Me" }]} onClose={onClose} onSent={() => {}} />);
    const bcc = screen.getByRole("combobox", { name: "Bcc" }) as HTMLInputElement;
    expect(bcc.value).toBe("hidden@x.com");
    fireEvent.change(bcc, { target: { value: "hidden@x.com, more@x.com" } });
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    fireEvent.click(await screen.findByRole("button", { name: "Save draft" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    const saved = mocked.mock.calls.find(([c]) => c === "save_draft")![1] as any;
    expect(saved.draft.bcc).toBe("hidden@x.com, more@x.com");
  });

  it("replies, reply-all and forwards start without blind copies (never carried over)", () => {
    for (const mode of ["reply", "replyAll", "forward"] as const) {
      cleanup();
      render(<Compose init={{ mode, replyTo: "m1", to: ["ana@corp.com"], cc: mode === "replyAll" ? ["bob@corp.com"] : [] }} accounts={[{ email: "me@corp.com", display_name: "Me" }]} onClose={() => {}} onSent={() => {}} />);
      expect(screen.queryByRole("combobox", { name: "Bcc" })).toBeNull();
      expect(screen.getByRole("button", { name: "Bcc" })).toBeTruthy();
      expect((screen.getByRole("combobox", { name: "Cc" }) as HTMLInputElement).value).toBe(mode === "replyAll" ? "bob@corp.com" : "");
    }
  });
});

describe("Quoting the message being answered", () => {
  const hostile = '<p>Original</p><img src="https://track.example/p.gif"><img src="x" onerror="alert(1)"><a href="javascript:alert(1)">bad</a><a href="https://ok.example/">ok</a><style>body{display:none}</style><form action="https://evil.example"><input name="pw"></form>';
  const reply = (quoteImages = false) => render(<Compose init={{ mode: "reply", to: ["a@b.com"], quoteHtml: hostile, quoteHeader: 'On Monday, <img src=x onerror=alert(1)> "Eve" <eve@x.com> wrote:', quoteImages }}
    accounts={[{ email: "me@corp.com", display_name: "Me" }]} onClose={() => {}} onSent={() => {}} />);
  const editor = () => document.querySelector(".editor") as HTMLElement;

  it("quotes a sanitised copy: no handlers, scripts, styles, forms, bad links or remote pictures", async () => {
    reply();
    await waitFor(() => expect(editor().querySelector("blockquote")).not.toBeNull());
    const q = editor().querySelector("blockquote")!;
    expect(q.textContent).toContain("Original");
    expect(q.querySelectorAll("style, form, input, script").length).toBe(0);
    expect(q.innerHTML).not.toMatch(/onerror|javascript:|track\.example/);
    expect(q.querySelector("a[href]")?.getAttribute("href")).toBe("https://ok.example/");
  });

  it("puts the header in as text, so a sender name can't add markup", async () => {
    reply();
    await waitFor(() => expect(editor().querySelector("blockquote")).not.toBeNull());
    const header = editor().querySelector("blockquote")!.previousElementSibling!;
    expect(header.textContent).toBe('On Monday, <img src=x onerror=alert(1)> "Eve" <eve@x.com> wrote:');
    expect(header.querySelector("img")).toBeNull();
  });

  it("keeps https pictures only when the user loaded the message's images", async () => {
    reply(true);
    await waitFor(() => expect(editor().querySelector("blockquote")).not.toBeNull());
    expect(editor().querySelector("blockquote img")?.getAttribute("src")).toBe("https://track.example/p.gif");
  });

  it("a reopened draft is sanitised too", async () => {
    render(<Compose init={{ mode: "new", draftId: "d1", html: '<div data-sig="1">Sig</div><p>Mine</p><img src=x onerror="alert(1)"><script>alert(1)</script>' }}
      accounts={[{ email: "me@corp.com", display_name: "Me" }]} onClose={() => {}} onSent={() => {}} />);
    await waitFor(() => expect(editor().textContent).toContain("Mine"));
    expect(editor().innerHTML).not.toMatch(/onerror|<script/);
    expect(editor().querySelector("[data-sig]")).not.toBeNull(); // the signature can still be swapped
  });
});
