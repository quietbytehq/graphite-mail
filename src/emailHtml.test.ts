// @vitest-environment jsdom
/**
 * The hostile-email suite: every fixture in test-fixtures/hostile-mail (also sent to a real mailbox by
 * scripts/send-test-mails.py) goes through the same code the reader uses, and nothing in the result may run or load.
 */
import { describe, expect, it } from "vitest";
import { FRAME_SANDBOX, buildDoc, framePolicy, safeLink, sanitizedFragment } from "./emailHtml";

const FIXTURES = import.meta.glob("../test-fixtures/hostile-mail/*.html", { query: "?raw", import: "default", eager: true }) as Record<string, string>;
const fixture = (name: string) => FIXTURES[Object.keys(FIXTURES).find((k) => k.includes(name))!];

const REMOTE = /(https?:)?\/\/|evil\.example/i;
const URL_ATTRS = ["src", "srcset", "background", "poster", "data", "action", "formaction", "codebase", "lowsrc", "dynsrc", "xlink:href"];

/** Parses the frame document without running or loading anything (an inert document, like DOMParser's). */
const parse = (doc: string) => new DOMParser().parseFromString(doc, "text/html");

/** Everything that would make a message run code, navigate, submit or contact a server. */
function dangers(d: Document, allowRemote = false): string[] {
  const out: string[] = [];
  const policies = d.querySelectorAll('meta[http-equiv="Content-Security-Policy"]');
  if (policies.length !== 1 || policies[0].getAttribute("content") !== framePolicy(allowRemote)) out.push("frame policy missing or changed");
  for (const tag of ["script", "noscript", "template", "iframe", "frame", "frameset", "object", "embed", "applet", "portal", "form", "input", "button", "textarea", "select", "base", "link", "svg", "math", "isindex"]) {
    if (d.getElementsByTagName(tag).length) out.push(`<${tag}>`);
  }
  for (const m of d.querySelectorAll("meta")) {
    const eq = m.getAttribute("http-equiv");
    if (eq && m !== policies[0]) out.push(`<meta http-equiv=${eq}>`);
  }
  for (const el of d.querySelectorAll("*")) {
    for (const a of el.attributes) {
      if (/^on/i.test(a.name)) out.push(`${el.tagName} ${a.name}`);
      if (URL_ATTRS.includes(a.name.toLowerCase())) {
        if (/^(javascript|vbscript|file|data:text)/i.test(a.value.replace(/\s/g, ""))) out.push(`${el.tagName} ${a.name}=${a.value}`);
        if (!allowRemote && REMOTE.test(a.value)) out.push(`remote ${el.tagName} ${a.name}=${a.value}`);
      }
      if (a.name === "style" && !allowRemote && REMOTE.test(a.value)) out.push(`remote style ${a.value}`);
    }
    if (el.hasAttribute("href")) {
      if (!/^(https?:|mailto:)/i.test(el.getAttribute("href")!)) out.push(`${el.tagName} href=${el.getAttribute("href")}`);
    }
    if ((el.tagName === "A" || el.tagName === "AREA") && el.getAttribute("target") !== "_blank") out.push(`${el.tagName} target=${el.getAttribute("target")}`);
  }
  for (const s of d.querySelectorAll("style")) {
    if (/@import/i.test(s.textContent ?? "")) out.push("@import");
    if (!allowRemote && REMOTE.test(s.textContent ?? "")) out.push(`remote css: ${s.textContent}`);
  }
  return out;
}

describe("hostile email fixtures", () => {
  it("has every fixture", () => {
    expect(Object.keys(FIXTURES).length).toBeGreaterThanOrEqual(8);
  });

  for (const [path, html] of Object.entries(FIXTURES)) {
    const name = path.split("/").pop()!;
    it(`${name}: nothing can run, navigate, submit or load`, () => {
      const { doc } = buildDoc(html, false, false);
      expect(dangers(parse(doc))).toEqual([]);
      expect(doc).not.toMatch(/__pwned/);
      // the dark-mode variant is the same document with one more style rule
      expect(dangers(parse(buildDoc(html, false, true).doc))).toEqual([]);
    });
    it(`${name}: with images loaded, still nothing runs and only https pictures remain`, () => {
      const d = parse(buildDoc(html, true, false).doc);
      expect(dangers(d, true)).toEqual([]);
      for (const el of d.querySelectorAll("[src]")) expect(el.getAttribute("src")).toMatch(/^(https:|data:image\/|cid:)/i);
    });
  }

  it("scripts and handlers are gone, but the readable text stays", () => {
    const d = parse(buildDoc(fixture("01-script"), false, false).doc);
    expect(d.body.textContent).toContain("Nothing in this message may run");
    expect(d.body.textContent).toContain("<script>this text is fine</script>");
    const e = parse(buildDoc(fixture("02-event"), false, false).doc);
    expect(e.body.textContent).toContain("Hover or click me");
    expect(e.querySelector('a[href="https://example.com/"]')).not.toBeNull();
  });

  it("only http, https and mailto links keep their address, and every link asks for a new window", () => {
    const d = parse(buildDoc(fixture("03-links"), false, false).doc);
    const links = [...d.querySelectorAll("a, area")];
    expect(links.length).toBe(22);
    const kept = links.map((a) => a.getAttribute("href")).filter(Boolean);
    expect(kept).toEqual([
      "https://example.com/top",
      "https://example.com/self",
      "https://example.com/parent",
      "https://example.com/named",
      "https://example.com/plain",
      "http://example.com/plain",
      "mailto:someone@example.com?subject=hi",
    ]);
    for (const a of links) {
      expect(a.getAttribute("target")).toBe("_blank");
      expect(a.getAttribute("rel")).toBe("noopener noreferrer");
    }
  });

  it("forms, meta refresh, base and link tags leave nothing behind", () => {
    const f = parse(buildDoc(fixture("04-forms"), false, false).doc);
    expect(f.querySelectorAll("form, input, button, textarea, select").length).toBe(0);
    const m = buildDoc(fixture("05-meta-refresh"), false, false).doc;
    expect(m).not.toMatch(/http-equiv="refresh"|evil\.example|set-cookie|default-src \*/i);
    expect(parse(m).querySelectorAll("meta").length).toBe(2); // our charset and policy only
    // the relative image can't be resolved against the email's (removed) base
    expect(parse(m).querySelector("img")?.hasAttribute("src")).toBe(false);
  });

  it("tracking pixels and remote resources are blocked and counted until the user loads images", () => {
    const html = fixture("06-tracking");
    const blocked = buildDoc(html, false, false);
    expect(blocked.blocked).toBeGreaterThanOrEqual(15);
    expect(blocked.doc).not.toMatch(/evil\.example/);
    expect(dangers(parse(blocked.doc))).toEqual([]);
    // loaded: https pictures come back, plain http, @import, fonts and media addresses still don't
    const loaded = buildDoc(html, true, false);
    expect(loaded.doc).toContain("https://evil.example/pixel.gif?u=you");
    expect(loaded.doc).toContain("https://evil.example/td-style.png");
    expect(loaded.doc).not.toContain("pixel-http.gif");
    expect(loaded.doc).not.toMatch(/import\.css|import2\.css|font\.woff2|@font-face/);
    expect(framePolicy(true)).toBe("default-src 'none'; img-src data: https:; style-src 'unsafe-inline'; form-action 'none'; base-uri 'none'");
    expect(framePolicy(false)).not.toMatch(/https:|http:|\*/);
  });

  it("inline (cid:/data:) pictures still show while remote ones are blocked", () => {
    const { doc, blocked } = buildDoc(fixture("08-inline"), false, false);
    const d = parse(doc);
    expect(blocked).toBe(0);
    expect(d.querySelector('img[alt="data image"]')?.getAttribute("src")).toMatch(/^data:image\/png;base64,/);
    expect(d.querySelector('img[alt="cid image"]')?.getAttribute("src")).toBe("cid:logo@example");
  });

  it("frames, objects, embeds, SVG and MathML are removed", () => {
    const { doc } = buildDoc(fixture("07-embeds"), false, false);
    expect(doc).not.toMatch(/evil\.example|srcdoc|xlink/);
  });

  it("the frame never gets scripts or the app's origin", () => {
    const flags = FRAME_SANDBOX.split(/\s+/);
    expect(flags).toEqual(["allow-popups"]);
    expect(flags).not.toContain("allow-scripts");
    expect(flags).not.toContain("allow-same-origin");
    expect(flags).not.toContain("allow-popups-to-escape-sandbox");
    expect(flags).not.toContain("allow-top-navigation");
    expect(flags).not.toContain("allow-forms");
  });
});

describe("the reply editor's copy of an email", () => {
  it("is a sanitised fragment: no scripts, handlers, forms, styles or remote pictures", () => {
    for (const html of Object.values(FIXTURES)) {
      const holder = document.createElement("div");
      holder.append(sanitizedFragment(html, false));
      expect(holder.querySelectorAll("script, iframe, object, embed, form, input, button, meta, base, link, svg, style").length).toBe(0);
      for (const el of holder.querySelectorAll("*")) {
        for (const a of el.attributes) {
          expect(a.name).not.toMatch(/^on/i);
          if (a.name !== "href") expect(a.value).not.toMatch(/evil\.example/);
        }
      }
      for (const el of holder.querySelectorAll("[href]")) expect(el.getAttribute("href")).toMatch(/^(https?:|mailto:)/i);
      expect(holder.innerHTML).not.toMatch(/__pwned/);
    }
  });

  it("keeps https pictures only when the user loaded them", () => {
    const pixel = '<p>Hi<img src="https://t.example/p.gif"></p>';
    const off = document.createElement("div"); off.append(sanitizedFragment(pixel, false));
    const on = document.createElement("div"); on.append(sanitizedFragment(pixel, true));
    expect(off.querySelector("img")?.hasAttribute("src")).toBe(false);
    expect(on.querySelector("img")?.getAttribute("src")).toBe("https://t.example/p.gif");
  });
});

describe("link schemes", () => {
  it("allows http, https and mailto", () => {
    for (const ok of ["https://example.com", "http://example.com/a?b#c", "HTTPS://EXAMPLE.COM", "mailto:a@b.c", "MailTo:a@b.c"]) expect(safeLink(ok)).toBe(ok);
    expect(safeLink("  https://example.com ")).toBe("https://example.com");
  });
  it("refuses everything else, including disguised schemes", () => {
    for (const bad of [
      "javascript:alert(1)", "JAVASCRIPT:alert(1)", "java\tscript:alert(1)", "java\nscript:x", " \u000e javascript:x", "\u0000javascript:x",
      "data:text/html,<script>", "file:///etc/passwd", "vbscript:x", "blob:https://x/1", "ftp://x", "smb://x/y", "ms-msdt:/id",
      "tel:123", "about:blank", "chrome://settings", "//example.com", "/relative", "relative.html", "#frag", "", null, undefined,
    ]) expect(safeLink(bad as string)).toBeNull();
  });
});
