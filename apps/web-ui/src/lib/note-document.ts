import { unified } from "unified";
import remarkParse from "remark-parse";
import remarkGfm from "remark-gfm";
import { toString } from "mdast-util-to-string";

export interface NoteSection {
  line: number;
  title: string;
  points: string[];
}

const parser = unified().use(remarkParse).use(remarkGfm);

/** Read the same Markdown grammar as the document renderer, including fenced code. */
export function readNoteDocument(markdown: string): { title: string | null; sections: NoteSection[]; words: number } {
  const tree = parser.parse(markdown);
  const sections: NoteSection[] = [];
  let title: string | null = null;
  let current: NoteSection | undefined;
  for (const node of tree.children) {
    if (node.type === "heading" && node.depth === 1) {
      title ??= toString(node);
    } else if (node.type === "heading" && node.depth === 2) {
      current = { line: node.position?.start.line ?? 0, title: toString(node), points: [] };
      sections.push(current);
    } else if (current && node.type !== "code" && node.type !== "html") {
      const points = node.type === "list" ? node.children.map((item) => toString(item))
        : node.type === "table" ? node.children.slice(1).map((row) => row.children.map((cell) => toString(cell)).filter(Boolean).join(" · "))
        : [toString(node)];
      current.points.push(...points.map((point) => point.replace(/\s+/g, " ").trim()).filter(Boolean));
    }
  }
  return { title, sections, words: toString(tree).split(/\s+/).filter(Boolean).length };
}

export function sectionId(documentId: string, line: number): string {
  return `${documentId}-section-${line}`;
}

export function shortMapLabel(text: string): string {
  if (text.length <= 140) return text;
  const end = text.lastIndexOf(" ", 137);
  return `${text.slice(0, end > 80 ? end : 137)}…`;
}
