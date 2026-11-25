import { describe, expect, test } from "bun:test";
import { spokenText } from "./speech";

describe("replies read aloud", () => {
  test("without the reaction emoji, markdown marks, code blocks or link addresses", () => {
    expect(spokenText("✅ All **3** tests pass in `api`.")).toBe("All 3 tests pass in api.");
    expect(spokenText("🐾 See [the PR](https://github.com/x/y/pull/1) or https://example.com")).toBe("See the PR or a link");
    expect(spokenText("Run this:\n```sh\nnpm test\n```\nthen tell me.")).toBe("Run this: (code) then tell me.");
    expect(spokenText("⚠️")).toBe("");
  });
});
