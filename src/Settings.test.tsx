// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import Settings from "./Settings";
import { SIGN_IN_TIMEOUT_MS } from "./accounts";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: vi.fn(), open: vi.fn() }));

const mocked = vi.mocked(invoke);
/** Answers the app's backend calls; `googleOk` is whether the build has the Google client secret. */
function backend(googleOk: boolean, extra: Record<string, (args?: unknown) => unknown> = {}) {
  mocked.mockImplementation(async (cmd: string, args?: unknown) => {
    if (cmd in extra) return extra[cmd](args);
    switch (cmd) {
      case "google_sign_in_available": return googleOk;
      case "get_notifications": return true;
      case "get_signature": return "";
      case "data_folder_info": return { path: "/data", size_bytes: 1 << 20 };
      default: return null;
    }
  });
}
const accounts = [{ email: "work@corp.com", display_name: "Work", provider: "microsoft" }];
const open = () => render(<Settings accounts={accounts} onClose={() => {}} onAccountsChanged={() => {}} theme="system" onTheme={() => {}} />);
const appPasswordButton = () => screen.getByRole("button", { name: "Add Gmail or other email (app password)" });
const called = (cmd: string) => mocked.mock.calls.some(([c]) => c === cmd);

const off = (el: HTMLElement) => (el as HTMLButtonElement).disabled;

beforeEach(() => {
  mocked.mockReset();
  Element.prototype.scrollIntoView = vi.fn();
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("Settings → Accounts", () => {
  it("toggles the app-password form, scrolling to it and focusing the address field", async () => {
    backend(true);
    open();
    const btn = appPasswordButton();
    expect(off(btn)).toBe(false);
    expect(btn.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(btn);
    const form = screen.getByRole("form", { name: "Email with an app password" });
    expect(btn.getAttribute("aria-expanded")).toBe("true");
    expect(document.activeElement).toBe(screen.getByLabelText("Gmail address"));
    expect(Element.prototype.scrollIntoView).toHaveBeenCalled();
    expect(form).toBeTruthy();
    // Clicking it again (it never looks switched off) closes the form.
    expect(off(appPasswordButton())).toBe(false);
    fireEvent.click(appPasswordButton());
    expect(screen.queryByRole("form", { name: "Email with an app password" })).toBeNull();
    expect(appPasswordButton().getAttribute("aria-expanded")).toBe("false");
  });

  it("keeps the app-password button usable while a Google sign-in waits, and Cancel gets out", async () => {
    let fail: (e: unknown) => void = () => {};
    backend(true, { add_google_account: () => new Promise((_, reject) => { fail = reject; }) });
    open();
    fireEvent.click(await screen.findByRole("button", { name: "Add Google account" }));
    expect(off(screen.getByRole("button", { name: "Waiting for browser…" }))).toBe(true);
    expect(off(screen.getByRole("button", { name: "Add Microsoft account" }))).toBe(true);
    expect(off(appPasswordButton())).toBe(false);
    fireEvent.click(appPasswordButton());
    expect(screen.getByRole("form", { name: "Email with an app password" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Cancel sign-in" }));
    expect(called("cancel_sign_in")).toBe(true);
    expect(screen.getByText("Sign-in cancelled.")).toBeTruthy();
    expect(off(screen.getByRole("button", { name: "Add Google account" }))).toBe(false);
    expect(off(screen.getByRole("button", { name: "Add Microsoft account" }))).toBe(false);
    // The cancelled sign-in finishes in the background; it must not bring an error (or "waiting") back.
    await act(async () => fail("Sign-in was cancelled."));
    expect(screen.queryByText("Sign-in was cancelled.")).toBeNull();
    expect(screen.queryByRole("button", { name: "Waiting for browser…" })).toBeNull();
  });

  it("gives up on a browser sign-in after about two minutes", async () => {
    backend(true, { sign_in: () => new Promise(() => {}) });
    open();
    await screen.findByRole("button", { name: "Add Google account" });
    vi.useFakeTimers();
    fireEvent.click(screen.getByRole("button", { name: "Add Microsoft account" }));
    expect(off(screen.getByRole("button", { name: "Waiting for browser…" }))).toBe(true);
    act(() => { vi.advanceTimersByTime(SIGN_IN_TIMEOUT_MS - 1000); });
    expect(called("cancel_sign_in")).toBe(false);
    act(() => { vi.advanceTimersByTime(1000); });
    expect(called("cancel_sign_in")).toBe(true);
    expect(screen.getByText(/took too long/)).toBeTruthy();
    expect(off(screen.getByRole("button", { name: "Add Microsoft account" }))).toBe(false);
    expect(SIGN_IN_TIMEOUT_MS).toBe(120_000);
  });

  it("replaces Google sign-in with a note in builds without the Google client secret", async () => {
    backend(false);
    open();
    expect(await screen.findByText("Google sign-in coming soon. Use an app password for now.")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Add Google account" })).toBeNull();
    expect(off(appPasswordButton())).toBe(false);
  });

  it("offers Google sign-in when the build has it", async () => {
    backend(true);
    open();
    expect(await screen.findByRole("button", { name: "Add Google account" })).toBeTruthy();
    await waitFor(() => expect(screen.queryByText(/coming soon/)).toBeNull());
  });

  it("removes one account's cached contacts (Microsoft 365: downloaded; Gmail app password: local)", async () => {
    backend(true);
    render(<Settings accounts={[...accounts, { email: "me@gmail.com", display_name: "Gm", provider: "imap" }]} onClose={() => {}} onAccountsChanged={() => {}} theme="system" onTheme={() => {}} />);
    const buttons = screen.getAllByRole("button", { name: "Remove cached contacts" });
    expect(buttons).toHaveLength(2); // v1.11: the Gmail (app password) account has local contacts too
    fireEvent.click(buttons[0]);
    await waitFor(() => expect(mocked).toHaveBeenCalledWith("remove_cached_contacts", { email: "work@corp.com" }));
    expect(await screen.findByText(/Cached contacts of work@corp.com were removed/)).toBeTruthy();
    fireEvent.click(buttons[1]);
    await waitFor(() => expect(mocked).toHaveBeenCalledWith("remove_cached_contacts", { email: "me@gmail.com" }));
    expect(await screen.findByText(/People you've emailed and imported contacts of me@gmail.com were removed/)).toBeTruthy();
  });
});

describe("Settings → admin approval", () => {
  it("shows the explanation and the copy button when adding a Microsoft account needs admin approval", async () => {
    backend(false, { sign_in: () => Promise.reject("admin-approval:Your organisation requires an administrator to approve Graphite Mail."), admin_consent_link: () => "https://example.test/consent" });
    open();
    fireEvent.click(await screen.findByRole("button", { name: "Add Microsoft account" }));
    expect(await screen.findByText("Your organisation needs to approve Graphite Mail")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Copy link for your IT admin" })).toBeTruthy();
    expect(screen.queryByText(/admin-approval:/)).toBeNull();
  });
});
