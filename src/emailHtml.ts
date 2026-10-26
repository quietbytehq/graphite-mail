/**
 * Everything that turns untrusted HTML (email bodies, calendar descriptions) into something the app shows.
 *
 * - Bodies are only ever shown inside a sandboxed iframe (`FRAME_SANDBOX`: no scripts, no same-origin), never in the
 *   app's own DOM. The reply editor gets a sanitised DOM fragment, never an HTML string.
 * - DOMPurify removes scripts, event handlers, forms, frames/objects/embeds, meta/base/link tags and SVG/MathML.
 * - Links keep an address only if it is http, https or mailto, and always ask for a new window: the app (Rust side,
 *   `on_new_window`) refuses every window and opens the address in the system browser instead.
 * - Remote images and other remote resources are removed unless the user loaded them; inline (cid: → data:) pictures
 *   always show. A Content-Security-Policy inside the frame blocks anything that slips through.
 */
import DOMPurify from "dompurify";

/** The email frame may only ask for popups (which the app turns into checked links). No scripts, no same-origin. */
export const FRAME_SANDBOX = "allow-popups";

const LINK = /^(https?:|mailto:)/i;
const REMOTE = /^(https?:)?\/\//i;
/** Pictures that are part of the message itself. (cid: pictures are turned into data: by the Rust side.) */
const INLINE_IMG = /^(cid:|data:image\/)/i;
const URL_ATTRS = ["src", "background", "poster", "lowsrc", "dynsrc"];

/** The address a link may keep: http, https or mailto, else nothing. Blanks and control characters are ignored
 * the way browsers ignore them ("java\tscript:" is still javascript:). */
export function safeLink(href: string | null | undefined): string | null {
  if (!href) return null;
  // eslint-disable-next-line no-control-regex
  const v = href.replace(/[\u0000- \u007f-\u009f]/g, "");
  return LINK.test(v) ? href.trim() : null;
}

const SANITIZE = {
  FORBID_TAGS: [
    "script", "noscript", "template", "form", "input", "button", "select", "option", "textarea", "fieldset",
    "iframe", "frame", "frameset", "object", "embed", "applet", "portal", "meta", "base", "link", "svg", "math",
  ],
  FORBID_ATTR: ["action", "formaction", "srcdoc", "ping", "download", "http-equiv", "target", "srcset", "xlink:href"],
};

type Opts = { allowRemote: boolean };

/** Removes remote addresses from CSS unless allowed (url(), image-set strings); @import and @font-face always go.
 * Counts the remote addresses it removed. */
function scrubCss(css: string, allowRemote: boolean, count: () => void): string {
  // Style sheets and fonts are never fetched (pictures are the only remote thing a user can allow).
  let out = css.replace(/@import[^;]*;?/gi, () => { count(); return ""; });
  out = out.replace(/@font-face\s*\{[^}]*\}/gi, (rule) => { if (REMOTE.test(rule)) count(); return ""; });
  out = out.replace(/url\(\s*(['"]?)(.*?)\1\s*\)/gi, (whole, _q, target: string) => {
    const t = target.trim();
    if (/^data:/i.test(t)) return whole;
    if (REMOTE.test(t)) {
      if (allowRemote && /^https:/i.test(t)) return whole;
      count();
    }
    return "none";
  });
  if (!allowRemote) {
    out = out.replace(/(['"])\s*(https?:)?\/\/[^'"]*\1/gi, () => { count(); return '""'; });
  }
  return out;
}

/** Sanitises untrusted HTML. Returns the cleaned document root (an inert element, not attached anywhere). */
function clean(html: string, { allowRemote }: Opts, whole: boolean) {
  let blocked = 0;
  const count = () => { blocked++; };
  // A private DOMPurify instance, so the hooks can't leak into other callers.
  const purify = DOMPurify(window);
  purify.addHook("uponSanitizeElement", (node) => {
    if (node.nodeName === "STYLE" && node.textContent) node.textContent = scrubCss(node.textContent, allowRemote, count);
  });
  purify.addHook("afterSanitizeAttributes", (node) => {
    const tag = node.nodeName.toUpperCase();
    if (node.hasAttribute("href")) {
      const keep = safeLink(node.getAttribute("href"));
      if (keep) node.setAttribute("href", keep);
      else node.removeAttribute("href");
    }
    if (tag === "A" || tag === "AREA") {
      // Every link click asks for a new window, whatever the email said: the app refuses the window and decides.
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer");
    }
    for (const attr of URL_ATTRS) {
      const v = node.getAttribute(attr)?.trim();
      if (!v) continue;
      if (INLINE_IMG.test(v)) continue;
      if (REMOTE.test(v)) {
        if (allowRemote && /^https:/i.test(v)) continue;
        count();
      }
      node.removeAttribute(attr); // remote (blocked), or file:, javascript:, relative … : never loaded
    }
    const style = node.getAttribute("style");
    if (style) node.setAttribute("style", scrubCss(style, allowRemote, count));
  });
  const root = purify.sanitize(html, { ...SANITIZE, WHOLE_DOCUMENT: whole, RETURN_DOM: true }) as HTMLElement;
  return { root, blocked };
}

const attrText = (el: Element | null) =>
  el ? [...el.attributes].map((a) => ` ${a.name}="${a.value.replace(/&/g, "&amp;").replace(/"/g, "&quot;")}"`).join("") : "";

/** The frame's own policy: nothing remote except (when allowed) https pictures; no scripts, fonts, media or forms. */
export function framePolicy(allowRemote: boolean) {
  return `default-src 'none'; img-src data:${allowRemote ? " https:" : ""}; style-src 'unsafe-inline'; form-action 'none'; base-uri 'none'`;
}

/** The full document for the email frame (`srcdoc`), and how many remote resources were held back. */
export function buildDoc(html: string, allowRemote: boolean, dark: boolean) {
  const { root, blocked } = clean(html, { allowRemote }, true);
  const head = root.querySelector("head");
  const body = root.querySelector("body");
  // Only the email's style sheets come from its head (title etc. are of no use in a frame).
  const styles = head ? [...head.querySelectorAll("style")].map((s) => s.outerHTML).join("") : "";
  const darkCss = dark ? "html{filter:invert(.9) hue-rotate(180deg);background:#fff} img,video,picture{filter:invert(1) hue-rotate(180deg)}" : "";
  const doc =
    `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${framePolicy(allowRemote)}">` +
    `<style>body{font-family:system-ui,sans-serif;font-size:14px;margin:16px 20px;word-wrap:break-word} ${darkCss}</style>${styles}</head>` +
    `<body${attrText(body)}>${body ? body.innerHTML : root.innerHTML}</body></html>`;
  return { doc, blocked };
}

/** Untrusted HTML as a sanitised fragment for the reply editor (inserted as nodes, never as an HTML string). */
export function sanitizedFragment(html: string, allowRemote: boolean): DocumentFragment {
  const { root } = clean(html, { allowRemote }, false);
  root.querySelectorAll("style").forEach((s) => s.remove()); // the email's styles would restyle the whole app
  const frag = document.createDocumentFragment();
  frag.append(...root.childNodes);
  return frag;
}

/** Plain text for an HTML context. */
export const escapeHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
