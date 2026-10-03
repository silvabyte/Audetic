import { test } from "node:test";
import assert from "node:assert/strict";
import { renderToStaticMarkup } from "react-dom/server";
import { readNoteDocument, shortMapLabel } from "../src/lib/note-document";
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

test("summary maps bound visible detail and link readers back to the full source section", () => {
  const points = Array.from({ length: 8 }, (_, i) => `Point ${i}: ${"A long grounded explanation. ".repeat(10)}`);
  const html = renderToStaticMarkup(<NoteMindMap title="Release review" sections={[{ line: 3, title: "Scope", points }]} onReadSection={() => {}} />);
  assert.match(html, /Read all 8 points/);
  assert.match(html, /aria-expanded="true"/);
  assert.doesNotMatch(html, /Point 5/);
  assert.ok(shortMapLabel(points[0]!).length <= 140);
  assert.equal(shortMapLabel("Short idea"), "Short idea");
});
