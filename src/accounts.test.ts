import { describe, expect, it } from "vitest";
import { cleanAppPassword, isGmail, looksLikeEmail, providerLabel } from "./accounts";

describe("app password accounts", () => {
  it("strips the spaces Google shows in an app password", () => {
    expect(cleanAppPassword("abcd efgh ijkl mnop")).toBe("abcdefghijklmnop");
    expect(cleanAppPassword(" abcd\tefgh\nijkl  mnop ")).toBe("abcdefghijklmnop");
    expect(cleanAppPassword("abcdefghijklmnop")).toHaveLength(16);
  });
  it("checks the address before asking Gmail", () => {
    expect(looksLikeEmail("name@gmail.com")).toBe(true);
    expect(looksLikeEmail(" name@gmail.com ")).toBe(true);
    expect(looksLikeEmail("name")).toBe(false);
    expect(looksLikeEmail("name@gmail")).toBe(false);
    expect(looksLikeEmail("na me@gmail.com")).toBe(false);
  });
  it("treats both kinds of Gmail account as Gmail", () => {
    expect(isGmail("google")).toBe(true);
    expect(isGmail("imap")).toBe(true);
    expect(isGmail("microsoft")).toBe(false);
    expect(isGmail(undefined)).toBe(false);
    expect(providerLabel("imap")).toBe("Gmail (app password)");
    expect(providerLabel("imap", false, { preset: "yahoo" })).toBe("Yahoo Mail (IMAP)");
    expect(providerLabel("imap", false, { preset: "custom" })).toBe("Other email (IMAP)");
    expect(providerLabel(undefined)).toBe("Microsoft 365");
    expect(providerLabel(undefined, true)).toBe("Microsoft account (personal)");
  });
});

describe("connect hint", () => {
  it("names what is missing", async () => {
    const { connectHint } = await import("./accounts");
    expect(connectHint("", "")).toBe("Enter both fields to continue");
    expect(connectHint("me@gmail.com", "   ")).toBe("Enter both fields to continue");
    expect(connectHint("me@gmail.com", "abcd")).toBe("4 of 16 letters");
    expect(connectHint("me@gmail.com", "abcd efgh ijkl mnop")).toBeNull();
    expect(connectHint("me@yahoo.com", "abcd", false)).toBeNull(); // other providers: any password
    expect(connectHint("me", "abcd", false)).toMatch(/full email address/);
  });
});
