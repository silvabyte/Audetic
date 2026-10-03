import { test } from "node:test";
import assert from "node:assert/strict";
import { renderToStaticMarkup } from "react-dom/server";
import { readNoteDocument } from "../src/lib/note-document";
import { readNoteMap, toMarkmapData, type NoteMapNode } from "../src/lib/note-map";
import { ArtifactContent } from "../src/components/artifact-content";
import { NoteMindMap } from "../src/components/note-mind-map";
import { TranscriptPlayer } from "../src/components/transcript-player";

test("document outline uses parsed headings, ignores code, and keeps duplicate headings distinct", () => {
  const markdown = "# A **useful** note\n\n## Next steps\n\n- **Ana:** ship the prototype\n\n```md\n## Not a section\n```\n\n## Next steps\n\n| Owner | Task |\n| --- | --- |\n| Jo | Test the prototype |\n";
  const document = readNoteDocument(markdown);
  assert.equal(document.title, "A useful note");
  assert.deepEqual(document.sections.map((section) => section.title), ["Next steps", "Next steps"]);
  assert.notEqual(document.sections[0]?.line, document.sections[1]?.line);
  assert.deepEqual(document.sections[0]?.points, ["Ana: ship the prototype"]);
  assert.deepEqual(document.sections[1]?.points, ["Jo · Test the prototype"]);
  const html = renderToStaticMarkup(<ArtifactContent markdown={markdown} documentId="test-document" outline />);
  for (const section of document.sections) {
    assert.ok(html.includes(`href="#test-document-section-${section.line}"`));
    assert.ok(html.includes(`id="test-document-section-${section.line}"`));
  }
});

test("source timestamps are seek buttons only when valid and within the recording", () => {
  const html = renderToStaticMarkup(<ArtifactContent markdown="[00:30](#t=30) [10:00](#t=600) [Invalid](#t=-1)" onSeek={() => {}} duration={90} />);
  assert.equal((html.match(/<button/g) ?? []).length, 1);
  assert.match(html, /Seek to 00:30/);
  assert.doesNotMatch(html, /Seek to 10:00/);
  const withoutAudio = renderToStaticMarkup(<ArtifactContent markdown="[00:30](#t=30)" />);
  assert.doesNotMatch(withoutAudio, /<button/);
});

test("transcript search treats punctuation literally and preserves original seek timestamps", () => {
  const html = renderToStaticMarkup(<TranscriptPlayer noteId={1} text="" segments={[{ start: 0, end: 10, text: "Nothing here" }, { start: 30, end: 45, text: "Use C++ for this prototype." }]} onSeek={() => {}} query="c++" />);
  assert.doesNotMatch(html, /Nothing here/);
  assert.match(html, /Seek to 0:30/);
  assert.match(html, /<mark[^>]*>C\+\+<\/mark>/);
});

test("summary maps retain every point and its full wording, with source navigation and an outline fallback", () => {
  const points = Array.from({ length: 8 }, (_, i) => `Point ${i}: ${"A long grounded explanation. ".repeat(10)}`);
  const markdown = `# Release review\n\n## Scope\n\n${points.map((point) => `- ${point}`).join("\n")}`;
  const tree = readNoteMap(markdown);
  assert.deepEqual(tree.children[0]?.children.map((node) => node.label), points.map((point) => point.trim()));
  const html = renderToStaticMarkup(<NoteMindMap markdown={markdown} theme="light" onReadSection={() => {}} />);
  assert.match(html, /Browse as an outline/);
  assert.match(html, /Point 7/);
  assert.match(html, /Read in summary/);
  assert.match(html, /Expand all/);
});

test("map preserves nested topics, lists, bold lead-ins and labeled action-table fields", () => {
  const markdown = "# Launch\n\nIntro context.\n\n## Delivery\n\n### Constraints\n\n- **Capacity:** Two engineers.\n  - No weekend work.\n\n## Next steps\n\n| Owner | Next step | When | Evidence |\n| --- | --- | --- | --- |\n| Ana | Ship pilot | Friday | Confirmed at 02:15 |\n\n## Delivery\n\nA second topic with the same name.";
  const tree = readNoteMap(markdown);
  assert.equal(tree.children[0]?.label, "Intro context.");
  const constraint = tree.children[1]?.children[0];
  assert.equal(constraint?.label, "Constraints");
  assert.equal(constraint?.children[0]?.label, "Capacity:");
  assert.deepEqual(constraint?.children[0]?.children.map((node) => node.label), ["Two engineers.", "No weekend work."]);
  const task = tree.children[2]?.children[0];
  assert.equal(task?.label, "Ship pilot");
  assert.deepEqual(task?.children.map((node) => node.label), ["Owner: Ana", "When: Friday", "Evidence: Confirmed at 02:15"]);
  assert.notEqual(tree.children[1]?.line, tree.children[3]?.line);
  const html = renderToStaticMarkup(<ArtifactContent markdown={markdown} documentId="source" />);
  function checkAnchors(node: NoteMapNode): void {
    assert.ok(html.includes(`id="source-section-${node.line}"`) || html.includes(`data-source-line="${node.line}"`), `Missing source line ${node.line}`);
    node.children.forEach(checkAnchors);
  }
  checkAnchors(tree);
});

test("map handles heading-free documents and never passes source HTML or link URLs to the canvas", () => {
  const tree = readNoteMap('A note with <img src=x onerror=alert(1)> and [unsafe](javascript:alert).\n\n- Literal `<script>` & "quotes".\n\n```md\n## Not a topic\n```\n\n<script>alert(1)</script>');
  assert.equal(tree.label, "Audio note");
  assert.equal(tree.children.length, 2);
  const data = toMarkmapData(tree);
  const content = [data.content, ...data.children.map((child) => child.content)].join("");
  assert.doesNotMatch(content, /<script>|<img|javascript:/);
  assert.match(content, /&lt;script&gt;/);
  assert.match(content, /&amp;/);
  assert.match(content, /data-source-line/);
});
