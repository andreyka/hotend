// Hotend's text is rendered from a safe subset of Markdown (DESIGN_SPEC 4.3.1):
// paragraphs, line breaks, bold, italic, lists and inline code. Headings render as bold
// body text. Links are shown as their text and cannot be clicked, images are dropped, raw
// HTML is shown as text. Nothing in a message can become a button: this module builds
// React text nodes only and never sets HTML.
import { Fragment, type ReactNode } from "react";

export type Block =
  | { type: "paragraph"; lines: string[] }
  | { type: "heading"; text: string }
  | { type: "list"; ordered: boolean; items: string[] };

const BULLET = /^\s*[-*•]\s+(.*)$/;
const NUMBERED = /^\s*\d+[.)]\s+(.*)$/;
const HEADING = /^\s{0,3}#{1,6}\s+(.*?)\s*#*\s*$/;

/** The blocks of a message. Text that is still streaming parses like any other. */
export function blocksOf(text: string): Block[] {
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  const flush = () => {
    if (paragraph.length > 0) blocks.push({ type: "paragraph", lines: paragraph });
    if (list) blocks.push({ type: "list", ...list });
    paragraph = [];
    list = null;
  };
  for (const line of text.replaceAll("\r\n", "\n").split("\n")) {
    if (line.trim() === "") {
      flush();
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      flush();
      blocks.push({ type: "heading", text: heading[1] ?? "" });
      continue;
    }
    const bullet = BULLET.exec(line);
    const numbered = bullet ? null : NUMBERED.exec(line);
    const item = bullet?.[1] ?? numbered?.[1];
    if (item !== undefined) {
      const ordered = numbered !== null;
      if (paragraph.length > 0 || (list && list.ordered !== ordered)) flush();
      list ??= { ordered, items: [] };
      list.items.push(item);
      continue;
    }
    if (list) {
      // A line under an item continues it.
      const last = list.items.length - 1;
      list.items[last] = `${list.items[last] ?? ""} ${line.trim()}`;
      continue;
    }
    paragraph.push(line.trim());
  }
  flush();
  return blocks;
}

const INLINE = /(!\[[^\]]*\]\([^)]*\))|(\[([^\]]+)\]\([^)]*\))|(`[^`]+`)|(\*\*[^*]+\*\*)|(__[^_]+__)|(\*[^*\s][^*]*\*)|(\b_[^_\s][^_]*_\b)/g;

/** Inline marks of one line as React nodes. */
export function inline(text: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let last = 0;
  let key = 0;
  for (const match of text.matchAll(INLINE)) {
    const [whole, image, link, linkText, code, bold, boldAlt, italic, italicAlt] = match;
    const at = match.index;
    if (at > last) nodes.push(text.slice(last, at));
    last = at + whole.length;
    if (image) continue;
    if (link) nodes.push(linkText);
    else if (code) {
      nodes.push(
        <code key={key++} className="t-mono rounded-tag bg-app-subtle px-1 py-px">
          {code.slice(1, -1)}
        </code>,
      );
    } else if (bold || boldAlt) {
      nodes.push(
        <strong key={key++} className="font-semibold">
          {(bold ?? boldAlt ?? "").slice(2, -2)}
        </strong>,
      );
    } else {
      nodes.push(<em key={key++}>{(italic ?? italicAlt ?? "").slice(1, -1)}</em>);
    }
  }
  if (last < text.length) nodes.push(text.slice(last));
  return nodes;
}

/** A message of Hotend's, as body text. */
export function Markdown({ text }: { text: string }) {
  return (
    <div className="t-body space-y-2 break-words text-app-text">
      {blocksOf(text).map((block, index) => {
        if (block.type === "heading") {
          return (
            <p key={index} className="font-semibold">
              {inline(block.text)}
            </p>
          );
        }
        if (block.type === "list") {
          const List = block.ordered ? "ol" : "ul";
          return (
            <List key={index} className={block.ordered ? "list-decimal space-y-1 pl-5" : "list-disc space-y-1 pl-5"}>
              {block.items.map((item, itemIndex) => (
                <li key={itemIndex} className="pl-0.5 marker:text-app-text-3">
                  {inline(item)}
                </li>
              ))}
            </List>
          );
        }
        return (
          <p key={index}>
            {block.lines.map((line, lineIndex) => (
              <Fragment key={lineIndex}>
                {lineIndex > 0 ? <br /> : null}
                {inline(line)}
              </Fragment>
            ))}
          </p>
        );
      })}
    </div>
  );
}
