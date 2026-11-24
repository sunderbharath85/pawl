import { describe, expect, test } from "bun:test";
import { QR_MARGIN, qrPath, qrSvg, type QrMatrix } from "./qr";

const grid = (rows: string[]): QrMatrix => ({ size: rows.length, get: (r, c) => rows[r]![c] === "#" });

describe("qrPath", () => {
  test("one rectangle per run of dark modules, shifted by the margin", () => {
    expect(qrPath(grid(["##.", "...", "#.#"]))).toBe("M0 0h2v1h-2zM0 2h1v1h-1zM2 2h1v1h-1z");
    expect(qrPath(grid(["#"]), 4)).toBe("M4 4h1v1h-1z");
    expect(qrPath(grid(["..", ".."]))).toBe("");
  });
});

describe("qrSvg", () => {
  const uri =
    "pawl://peer?v=1&mid=winpc0000001&pk=cGstd2lucGMcGstd2lucGMcGstd2lucGMcGstd2lucGM&relay=wss://relay.example.dev" +
    "&rt=AAAAAAAAAAAAAAAAAAAAAA&rk=k3y_SECRETk3y_SECRETk3y_SECRET&code=AAAAAAAAAAAAAAAAAAAAAA&exp=1900000000000";

  test("a phone invite fits a scannable code with a quiet zone", () => {
    const qr = qrSvg(uri);
    expect(qr.size).toBe(4 * qr.version + 17 + 2 * QR_MARGIN);
    expect(qr.version).toBeLessThanOrEqual(15);
    // The top-left finder pattern starts with 7 dark modules right after the quiet zone.
    expect(qr.path.startsWith(`M${QR_MARGIN} ${QR_MARGIN}h7v1h-7z`)).toBe(true);
    expect(qrSvg(uri)).toEqual(qr); // deterministic
    expect(qrSvg(`${uri}x`).path).not.toBe(qr.path);
  });

  test("text too long for any QR code throws", () => {
    expect(() => qrSvg("x".repeat(5000))).toThrow();
  });
});
