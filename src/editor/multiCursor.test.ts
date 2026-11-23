import { test } from "node:test";
import assert from "node:assert/strict";
import { EditorSelection, EditorState } from "@codemirror/state";
import { runTarget, statementMarks } from "./statementMarks.ts";
import { inSelection, selectedTexts } from "./multiCursor.ts";

const doc = "SELECT 1;\nGRANT SELECT ON db.* TO 'jackie'@'%';\nCREATE USER 'jackie'@'%';\nSELECT 3;";
const at = (s: string, n = 0) => {
  let i = -1;
  for (let k = 0; k <= n; k++) i = doc.indexOf(s, i + 1);
  return i;
};
const make = (ranges: { from: number; to: number }[]) =>
  EditorState.create({
    doc,
    extensions: [EditorState.allowMultipleSelections.of(true), statementMarks()],
    selection: EditorSelection.create(ranges.map((r) => EditorSelection.range(r.from, r.to))),
  });

test("several selections run the statements they sit in", () => {
  const a = at("jackie"), b = at("jackie", 1);
  const t = runTarget(make([{ from: a, to: a + 6 }, { from: b, to: b + 6 }]));
  assert.equal(t.kind, "selection");
  assert.equal(t.count, 2);
  assert.equal(t.text, "GRANT SELECT ON db.* TO 'jackie'@'%';\nCREATE USER 'jackie'@'%';");
});

test("several cursors in one statement run that statement", () => {
  const a = at("GRANT"), b = at("TO");
  const t = runTarget(make([{ from: a, to: a }, { from: b, to: b }]));
  assert.equal(t.kind, "statement");
  assert.equal(t.count, 1);
  assert.equal(t.text, "GRANT SELECT ON db.* TO 'jackie'@'%';");
});

test("one selection still runs just the selection", () => {
  const a = at("SELECT 3");
  assert.equal(runTarget(make([{ from: a, to: a + 8 }])).text, "SELECT 3");
});

test("selected texts and hit test cover every range", () => {
  const a = at("jackie"), b = at("jackie", 1);
  const s = make([{ from: a, to: a + 6 }, { from: b, to: b + 6 }]);
  assert.equal(selectedTexts(s), "jackie\njackie");
  assert.ok(inSelection(s, b + 3));
  assert.ok(!inSelection(s, 0));
});
