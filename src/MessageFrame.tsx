import { FRAME_SANDBOX } from "./emailHtml";

/** Shows a document made by `buildDoc` (an email body, a calendar description). The frame can't run scripts, isn't
 * same-origin with the app, and links in it can only ask for a new window, which the app turns into a checked link. */
export default function MessageFrame({ doc, title }: { doc: string; title: string }) {
  return <iframe sandbox={FRAME_SANDBOX} srcDoc={doc} title={title} referrerPolicy="no-referrer" />;
}
