import { unified } from "unified";
import remarkParse from "remark-parse";
import remarkGfm from "remark-gfm";
import { toString } from "mdast-util-to-string";
import type { RootContent } from "mdast";

export interface NoteMapNode {
  label: string;
  /** Markdown line for returning to the source block. */
  line: number;
  children: NoteMapNode[];
}

const parser = unified().use(remarkParse).use(remarkGfm);
const text = (node: Parameters<typeof toString>[0]) => toString(node).replace(/\s+/g, " ").trim();

/** Preserve summary prose and its hierarchy without another AI interpretation. */
export function readNoteMap(markdown: string, fallbackTitle = "Audio note"): NoteMapNode {
  const document = parser.parse(markdown);
  const title = document.children.find((node) => node.type === "heading" && node.depth === 1);
  const root: NoteMapNode = { label: title ? text(title) : fallbackTitle, line: title?.position?.start.line ?? document.children[0]?.position?.start.line ?? 1, children: [] };
  const stack = [{ depth: 0, node: root }];
  for (const block of document.children) {
    if (block === title) continue;
    if (block.type === "heading") {
      while (stack.length > 1 && stack[stack.length - 1]!.depth >= block.depth) stack.pop();
      const node: NoteMapNode = { label: text(block), line: block.position?.start.line ?? 1, children: [] };
      stack[stack.length - 1]!.node.children.push(node);
      stack.push({ depth: block.depth, node });
    } else {
      stack[stack.length - 1]!.node.children.push(...blockNodes(block));
    }
  }
  return root;
}

function blockNodes(block: RootContent): NoteMapNode[] {
  const line = block.position?.start.line ?? 1;
  if (block.type === "html" || block.type === "code" || block.type === "definition" || block.type === "thematicBreak") return [];
  if (block.type === "list") return block.children.flatMap((item) => {
    const nodes = item.children.flatMap(blockNodes);
    const first = nodes.shift();
    if (!first) return [];
    if (item.checked != null) first.label = `${item.checked ? "☑" : "☐"} ${first.label}`;
    first.children.push(...nodes);
    return [first];
  });
  if (block.type === "blockquote") return block.children.flatMap(blockNodes);
  if (block.type === "table") {
    const headers = block.children[0]?.children.map(text) ?? [];
    // Actions become the row label, while owner, timing and evidence remain named children.
    const actionIndex = headers.findIndex((header) => /^(?:task|action(?: item)?|next step|follow[- ]?up)$/i.test(header));
    const labelIndex = Math.max(0, actionIndex);
    return block.children.slice(1).map((row) => ({
      label: text(row.children[labelIndex] ?? row),
      line: row.position?.start.line ?? line,
      children: row.children.flatMap((cell, index) => index === labelIndex || !text(cell) ? [] : [{
        label: `${headers[index] || `Column ${index + 1}`}: ${text(cell)}`,
        line: row.position?.start.line ?? line,
        children: [],
      }]),
    }));
  }
  if (block.type === "paragraph" && block.children[0]?.type === "strong") {
    const lead = text(block.children[0]);
    const rest = text({ ...block, children: block.children.slice(1) }).replace(/^:\s*/, "");
    if (lead && rest) return [{ label: lead, line, children: [{ label: rest, line, children: [] }] }];
  }
  const label = text(block);
  return label ? [{ label, line, children: [] }] : [];
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]!);
}

/** Markmap accepts HTML. Only our own escaped text and numeric source pointers enter it. */
export function toMarkmapData(node: NoteMapNode, branch = -1): MarkmapData {
  return {
    content: `<button type="button" data-source-line="${node.line}" title="Read in summary">${escapeHtml(node.label)}</button>`,
    payload: { branch, label: node.label },
    children: node.children.map((child, index) => toMarkmapData(child, branch < 0 ? index : branch)),
  };
}

interface MarkmapData {
  content: string;
  payload: { branch: number; label: string };
  children: MarkmapData[];
}
