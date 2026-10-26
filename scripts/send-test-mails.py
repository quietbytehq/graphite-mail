#!/usr/bin/env python3
"""Send the hostile test emails (test-fixtures/hostile-mail) to a mailbox of your choice, to check by hand that
Graphite Mail shows them safely: nothing runs, nothing loads until "Load images", odd links do nothing, and
attachments with hostile names are saved safely.

The SMTP server, user and app password come only from environment variables. They are never written anywhere,
never printed, and never passed on the command line (where other users could see them in the process list):

    export GM_SMTP_HOST=smtp.example.com      # required
    export GM_SMTP_PORT=587                   # optional: 587 = STARTTLS (default), 465 = TLS from the start
    export GM_SMTP_USER=me@example.com        # required
    read -rs GM_SMTP_PASSWORD && export GM_SMTP_PASSWORD   # required: an app password, typed without echo

    python3 scripts/send-test-mails.py --to test-mailbox@example.com            # asks before sending
    python3 scripts/send-test-mails.py --to test-mailbox@example.com --only 03 06 --yes
    python3 scripts/send-test-mails.py --to x@example.com --dry-run            # show what would be sent

    unset GM_SMTP_PASSWORD   # afterwards

Only the Python standard library is used. TLS certificates are always verified; the script refuses to send
without TLS.
"""

from __future__ import annotations

import argparse
import os
import smtplib
import ssl
import sys
from email.message import EmailMessage
from email.utils import formatdate, make_msgid
from pathlib import Path
from urllib.parse import quote

FIXTURES = Path(__file__).resolve().parent.parent / "test-fixtures" / "hostile-mail"

# A 1x1 PNG (one blue pixel), used as the inline (cid:) picture fixture 08 refers to.
PNG_1X1 = bytes.fromhex(
    "89504e470d0a1a0a0000000d49484452000000010000000108060000001f"
    "15c4890000000d49444154789c63d0cfbff61f0004b90274e0c763ae0000"
    "000049454e44ae426082"
)

# Attachment names an attacker could use. The contents are harmless text.
HOSTILE_ATTACHMENTS = [
    "../../.bashrc",
    "..\\..\\Windows\\System32\\evil.dll",
    "/etc/cron.d/backdoor",
    "invoice‮fdp.exe",
    "CON.txt",
    "COM1",
    "run-me.sh",
    "evil.desktop",
    "setup.exe.",
    "report:Zone.Identifier.pdf",
    "tab\tbell\x07esc\x1b[31m.txt",
    "no-extension",
    "harmless.pdf",
]

ENV = ("GM_SMTP_HOST", "GM_SMTP_USER", "GM_SMTP_PASSWORD")


def settings() -> tuple[str, int, str, str]:
    missing = [k for k in ENV if not os.environ.get(k)]
    if missing:
        sys.exit(f"Missing environment variables: {', '.join(missing)} (see the top of this script).")
    port_text = os.environ.get("GM_SMTP_PORT", "587").strip() or "587"
    if not port_text.isdigit():
        sys.exit("GM_SMTP_PORT must be a number.")
    return os.environ["GM_SMTP_HOST"].strip(), int(port_text), os.environ["GM_SMTP_USER"].strip(), os.environ["GM_SMTP_PASSWORD"]


def fixture_messages(sender: str, to: str, only: list[str]) -> list[EmailMessage]:
    files = sorted(FIXTURES.glob("*.html"))
    if not files:
        sys.exit(f"No fixtures found in {FIXTURES}")
    out = []
    for f in files:
        number = f.name.split("-", 1)[0]
        if only and number not in only:
            continue
        msg = EmailMessage()
        msg["Subject"] = f"[Graphite hostile test {number}] {f.stem.split('-', 1)[1].replace('-', ' ')}"
        msg["From"] = sender
        msg["To"] = to
        msg["Date"] = formatdate(localtime=True)
        msg["Message-ID"] = make_msgid(domain="graphite-test.invalid")
        msg.set_content(f"Hostile test fixture {f.name}. View the HTML part in Graphite Mail.\n")
        msg.add_alternative(f.read_text(encoding="utf-8"), subtype="html")
        if "inline-images" in f.name:
            # The HTML refers to cid:logo@example; attach the picture so it can show.
            html_part = msg.get_payload()[1]
            html_part.add_related(PNG_1X1, maintype="image", subtype="png", cid="<logo@example>", filename="logo.png")
        out.append(msg)
    if not only or "09" in only:
        msg = EmailMessage()
        msg["Subject"] = "[Graphite hostile test 09] attachment names"
        msg["From"] = sender
        msg["To"] = to
        msg["Date"] = formatdate(localtime=True)
        msg["Message-ID"] = make_msgid(domain="graphite-test.invalid")
        msg.set_content(
            "Hostile test 09: attachments with hostile names. Opening a program or script must be refused; saving must\n"
            "put a safely named file into the folder you chose, never anywhere else.\n"
        )
        for name in HOSTILE_ATTACHMENTS:
            data = f"Harmless test file. Original name: {name!r}\n".encode()
            if any(ord(c) < 32 for c in name):
                # Python won't put control characters into a header; send the name RFC 2231-encoded, as an attacker would.
                msg.add_attachment(data, maintype="application", subtype="octet-stream", filename="placeholder")
                part = msg.get_payload()[-1]
                part.replace_header("Content-Disposition", "attachment; filename*=utf-8''" + quote(name, safe=""))
            else:
                msg.add_attachment(data, maintype="application", subtype="octet-stream", filename=name)
        out.append(msg)
    return out


def send(messages: list[EmailMessage], host: str, port: int, user: str, password: str) -> None:
    ctx = ssl.create_default_context()  # verifies certificates and host names
    server: smtplib.SMTP | None = None
    try:
        if port == 465:
            server = smtplib.SMTP_SSL(host, port, context=ctx, timeout=30)
        else:
            server = smtplib.SMTP(host, port, timeout=30)
            server.ehlo()
            if not server.has_extn("starttls"):
                sys.exit(f"{host}:{port} doesn't offer STARTTLS; refusing to send the password without TLS.")
            server.starttls(context=ctx)
            server.ehlo()
        server.login(user, password)
        for m in messages:
            server.send_message(m)
            print(f"sent: {m['Subject']}")
    # Error messages name the server only: never the password, and not the server's reply (it can echo input).
    except ssl.SSLCertVerificationError:
        sys.exit(f"{host}'s certificate couldn't be verified; nothing was sent.")
    except smtplib.SMTPAuthenticationError:
        sys.exit("The server refused the user name or app password.")
    except (smtplib.SMTPException, OSError) as e:
        sys.exit(f"Sending through {host}:{port} failed ({type(e).__name__}).")
    finally:
        if server is not None:
            try:
                server.quit()
            except (smtplib.SMTPException, OSError):
                pass


def main() -> None:
    ap = argparse.ArgumentParser(description="Send Graphite Mail's hostile test emails to a mailbox you choose.")
    ap.add_argument("--to", required=True, help="the mailbox that receives the test emails")
    ap.add_argument("--from", dest="sender", help="From address (default: GM_SMTP_USER)")
    ap.add_argument("--only", nargs="*", default=[], help="fixture numbers to send, e.g. 03 06 09 (default: all)")
    ap.add_argument("--dry-run", action="store_true", help="print the messages instead of sending them")
    ap.add_argument("--yes", action="store_true", help="don't ask before sending")
    args = ap.parse_args()
    if "@" not in args.to or any(c.isspace() for c in args.to):
        sys.exit("--to must be one email address.")

    if args.dry_run:
        sender = args.sender or os.environ.get("GM_SMTP_USER") or "sender@example.invalid"
        for m in fixture_messages(sender, args.to, args.only):
            print(m.as_string()[:1500], "\n" + "-" * 72)
        return

    host, port, user, password = settings()
    messages = fixture_messages(args.sender or user, args.to, args.only)
    print(f"About to send {len(messages)} hostile test emails from {args.sender or user} to {args.to} via {host}:{port}.")
    if not args.yes and input("Send them? [y/N] ").strip().lower() not in ("y", "yes"):
        sys.exit("Nothing sent.")
    send(messages, host, port, user, password)


if __name__ == "__main__":
    main()
