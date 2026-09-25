import assert from "node:assert/strict";
import { test } from "node:test";

import { timeKey } from "../src/client.ts";

test("timestamps order correctly regardless of fractional digits", () => {
  const ordered = ["2026-09-25T13:59:48Z", "2026-09-25T13:59:48.5Z", "2026-09-25T13:59:48.930Z", "2026-09-25T13:59:48.930962Z", "2026-09-25T13:59:49Z"];
  const keys = ordered.map(timeKey);
  for (let i = 1; i < keys.length; i++) assert.ok(keys[i]! > keys[i - 1]!, `${ordered[i]} > ${ordered[i - 1]}`);
  assert.ok(ordered[3]! < ordered[2]!, "string comparison would get this wrong");
});
