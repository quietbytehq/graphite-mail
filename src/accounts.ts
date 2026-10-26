/** Account types and small helpers shared by the screens that show or add accounts. */
import { imapLabel } from "./imapPresets";
export type Acct = { email: string; display_name: string; provider?: string; rules?: boolean; personal?: boolean; imap?: { preset?: string } | null };

/** Gmail through Google sign-in ("google") or through IMAP/SMTP with an app password ("imap"): labels are folders. */
export const isGmail = (provider?: string) => provider === "google" || provider === "imap";

export const providerLabel = (provider?: string, personal?: boolean, imap?: { preset?: string } | null) =>
  provider === "google" ? "Gmail" : provider === "imap" ? imapLabel(imap) : personal ? "Microsoft account (personal)" : "Microsoft 365";

/** A Gmail account reached through IMAP (no settings) rather than another provider's server. */
export const isGmailImap = (a: { provider?: string; imap?: { preset?: string } | null }) => a.provider === "imap" && !a.imap;

/** Google shows app passwords as four groups of four letters; the spaces are not part of the password. */
export const cleanAppPassword = (s: string) => s.replace(/\s+/g, "");

export const APP_PASSWORD_LENGTH = 16;

export const looksLikeEmail = (s: string) => /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(s.trim());

/** Why "Connect" can't be pressed yet (null when it can). Only Google's app passwords have a fixed shape. */
export function connectHint(email: string, password: string, gmail = true): string | null {
  const code = gmail ? cleanAppPassword(password) : password.trim();
  if (!email.trim() || !code) return "Enter both fields to continue";
  if (!looksLikeEmail(email)) return gmail ? "Enter your full Gmail address, for example name@gmail.com" : "Enter your full email address, for example name@example.com";
  if (gmail && code.length !== APP_PASSWORD_LENGTH) return `${code.length} of ${APP_PASSWORD_LENGTH} letters`;
  return null;
}

/** A browser sign-in that hasn't come back after this long is stopped, so the screen never stays stuck. */
export const SIGN_IN_TIMEOUT_MS = 2 * 60 * 1000;
