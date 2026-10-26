/** A browser sign-in that can be cancelled and gives up after SIGN_IN_TIMEOUT_MS. Shared by the first-run screen
 *  and Settings → Accounts, so both behave the same way:
 *  - each attempt gets a number; one that was cancelled or timed out can no longer change the screen (no late error,
 *    no "waiting" coming back), even if the backend only finishes much later;
 *  - cancelling tells the backend to stop its local redirect listener (`cancel_sign_in`);
 *  - leaving the screen clears the timer and retires the attempt, so nothing fires on an unmounted component. */
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SIGN_IN_TIMEOUT_MS } from "./accounts";

export const SIGN_IN_CANCELLED = "Sign-in cancelled.";
export const SIGN_IN_TIMED_OUT = "Sign-in took too long and was stopped. You can try again.";

export function useBrowserSignIn<K extends string>(onStop: (note: string) => void) {
  const [waiting, setWaiting] = useState<K | null>(null);
  const attempt = useRef(0);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const stopRef = useRef(onStop);
  stopRef.current = onStop;
  useEffect(() => () => { clearTimeout(timer.current); attempt.current++; }, []);

  /** Starts a sign-in of `kind`; `done` and `fail` only run while this attempt is still the current one. */
  async function run<T>(kind: K, go: () => Promise<T>, done: (r: T) => void, fail: (e: unknown) => void) {
    const mine = ++attempt.current;
    clearTimeout(timer.current);
    setWaiting(kind);
    timer.current = setTimeout(() => stop(SIGN_IN_TIMED_OUT), SIGN_IN_TIMEOUT_MS);
    try {
      const r = await go();
      if (mine === attempt.current) done(r);
    } catch (e) {
      if (mine === attempt.current) fail(e);
    } finally {
      if (mine === attempt.current) { clearTimeout(timer.current); setWaiting(null); }
    }
  }

  /** Gives up on the waiting sign-in (Cancel button or time limit), here and in the backend's redirect listener. */
  function stop(note: string) {
    attempt.current++;
    clearTimeout(timer.current);
    setWaiting(null);
    invoke("cancel_sign_in").catch(() => {});
    stopRef.current(note);
  }

  return { waiting, run, stop };
}

/** Microsoft's answer when an organisation's administrator must approve the app first. The backend marks that error
 *  with this prefix (see auth::ADMIN_APPROVAL_MARK); the screens then show the explanation and the link to send. */
export const ADMIN_APPROVAL_MARK = "admin-approval:";
export function adminApproval(error: unknown): string | null {
  const text = String(error ?? "");
  return text.startsWith(ADMIN_APPROVAL_MARK) ? text.slice(ADMIN_APPROVAL_MARK.length).trim() : null;
}
