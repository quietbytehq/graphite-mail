// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import AppPasswordForm from "./AppPasswordForm";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const mocked = vi.mocked(invoke);

const off = (el: HTMLElement) => (el as HTMLButtonElement).disabled;

beforeEach(() => { mocked.mockReset(); Element.prototype.scrollIntoView = vi.fn(); });
afterEach(cleanup);

const type = (label: string | RegExp, value: string) => fireEvent.change(screen.getByLabelText(label), { target: { value } });
const connect = () => screen.getByRole("button", { name: "Connect" });
const hint = () => document.getElementById("apppw-hint")?.textContent ?? null;

describe("Gmail app-password form", () => {
  it("says why Connect is inactive until both fields are filled in", () => {
    render(<AppPasswordForm onAdded={() => {}} />);
    expect(off(connect())).toBe(true);
    expect(hint()).toBe("Enter both fields to continue");
    expect(connect().getAttribute("aria-describedby")).toBe("apppw-hint");

    type("Gmail address", "me@gmail.com");
    expect(off(connect())).toBe(true);
    expect(hint()).toBe("Enter both fields to continue");

    type("App password", "abcd efgh ijkl");
    expect(off(connect())).toBe(true);
    expect(hint()).toBe("12 of 16 letters");

    type("Gmail address", "me");
    type("App password", "abcd efgh ijkl mnop");
    expect(off(connect())).toBe(true);
    expect(hint()).toMatch(/full Gmail address/);

    type("Gmail address", "me@gmail.com");
    expect(off(connect())).toBe(false);
    expect(hint()).toBeNull();
    expect(connect().getAttribute("aria-describedby")).toBeNull();
  });

  it("sends the code without Google's spaces", async () => {
    const added = vi.fn();
    mocked.mockResolvedValue({ email: "me@gmail.com", display_name: "me@gmail.com", provider: "imap" });
    render(<AppPasswordForm onAdded={added} />);
    type("Gmail address", " me@gmail.com ");
    type("App password", "abcd efgh ijkl mnop");
    fireEvent.click(connect());
    await waitFor(() => expect(added).toHaveBeenCalled());
    expect(mocked).toHaveBeenCalledWith("add_imap_account", { email: "me@gmail.com", password: "abcdefghijklmnop", settings: null });
  });

  it("puts the cursor in the address field when it opens", () => {
    render(<AppPasswordForm onAdded={() => {}} />);
    expect(document.activeElement).toBe(screen.getByLabelText("Gmail address"));
    expect(Element.prototype.scrollIntoView).toHaveBeenCalled();
  });
});

describe("Other email (IMAP) presets", () => {
  it("offers Yahoo, iCloud, Zoho, Fastmail, AOL and a custom server, each with its own help", () => {
    render(<AppPasswordForm onAdded={() => {}} />);
    const provider = screen.getByLabelText("Email provider") as HTMLSelectElement;
    expect([...provider.options].map((o) => o.textContent)).toEqual(["Gmail", "Yahoo Mail", "iCloud Mail", "Zoho Mail", "Fastmail", "AOL Mail", "Custom server"]);
    fireEvent.change(provider, { target: { value: "yahoo" } });
    expect(screen.getByText("Yahoo Mail with an app password")).toBeTruthy();
    expect(screen.getByText(/imap\.mail\.yahoo\.com:993/)).toBeTruthy();
    expect(screen.getByRole("list", { name: "How to create an app password" }).textContent).toContain("Generate app password");
    expect(screen.getByText(/mail only/)).toBeTruthy();
    fireEvent.change(provider, { target: { value: "icloud" } });
    expect(screen.getByText(/smtp\.mail\.me\.com:587 \(STARTTLS\)/)).toBeTruthy();
    fireEvent.change(provider, { target: { value: "custom" } });
    expect(screen.getByLabelText("IMAP server")).toBeTruthy();
    expect(screen.getByLabelText("SMTP server")).toBeTruthy();
  });

  it("sends the preset's servers and doesn't demand a 16-letter code", async () => {
    mocked.mockImplementation(async () => ({ email: "me@yahoo.com", display_name: "me@yahoo.com", provider: "imap", imap: { preset: "yahoo" } }));
    const onAdded = vi.fn();
    render(<AppPasswordForm onAdded={onAdded} />);
    fireEvent.change(screen.getByLabelText("Email provider"), { target: { value: "yahoo" } });
    type("Email address", "me@yahoo.com");
    type("App password", "shortpw");
    expect(hint()).toBeNull();
    expect(off(connect())).toBe(false);
    fireEvent.click(connect());
    await waitFor(() => expect(onAdded).toHaveBeenCalled());
    expect(mocked).toHaveBeenCalledWith("add_imap_account", { email: "me@yahoo.com", password: "shortpw", settings: {
      preset: "yahoo", imap_host: "imap.mail.yahoo.com", imap_port: 993, imap_tls: "tls", smtp_host: "smtp.mail.yahoo.com", smtp_port: 465, smtp_tls: "tls", username: "" } });
  });

  it("needs both server names for a custom server, and passes them with the user name", async () => {
    mocked.mockImplementation(async () => ({ email: "me@example.net", display_name: "me@example.net", provider: "imap", imap: { preset: "custom" } }));
    render(<AppPasswordForm onAdded={() => {}} />);
    fireEvent.change(screen.getByLabelText("Email provider"), { target: { value: "custom" } });
    type("Email address", "me@example.net");
    type("App password", "pw");
    expect(hint()).toBe("Enter the IMAP and SMTP server names");
    type("IMAP server", "mail.example.net");
    type("SMTP server", "mail.example.net");
    fireEvent.change(screen.getAllByLabelText("Security")[0], { target: { value: "starttls" } });
    fireEvent.change(screen.getAllByLabelText("Port")[0], { target: { value: "143" } });
    type(/User name/, "ann");
    expect(hint()).toBeNull();
    fireEvent.click(connect());
    await waitFor(() => expect(mocked).toHaveBeenCalled());
    expect(mocked.mock.calls[0][1]).toMatchObject({ settings: { preset: "custom", imap_host: "mail.example.net", imap_port: 143, imap_tls: "starttls", smtp_host: "mail.example.net", smtp_port: 465, smtp_tls: "tls", username: "ann" } });
  });
});
