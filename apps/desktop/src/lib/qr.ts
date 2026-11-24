// Pairing QR codes, drawn locally with `qrcode` (PLAN.md migration step 7). The invite (it holds the relay access key)
// never goes to a QR web service: the module grid becomes one SVG path rendered by React.
import { create } from "qrcode";

/** The part of qrcode's BitMatrix this needs (also easy to fake in tests). */
export interface QrMatrix {
  size: number;
  get(row: number, col: number): number | boolean;
}

/** The quiet zone around the code, in modules (the QR spec asks for 4). */
export const QR_MARGIN = 4;

/**
 * A QR module grid as one SVG path: every run of dark modules in a row becomes a 1-module-high rectangle, shifted by
 * `margin` so the quiet zone stays light.
 */
export function qrPath(m: QrMatrix, margin = 0): string {
  let d = "";
  for (let r = 0; r < m.size; r++) {
    let c = 0;
    while (c < m.size) {
      if (!m.get(r, c)) {
        c++;
        continue;
      }
      const start = c;
      while (c < m.size && m.get(r, c)) c++;
      const w = c - start;
      d += `M${start + margin} ${r + margin}h${w}v1h-${w}z`;
    }
  }
  return d;
}

export interface QrSvg {
  /** viewBox width and height (modules plus the quiet zone on both sides). */
  size: number;
  path: string;
  /** QR version (1..40): 4 × version + 17 modules a side. */
  version: number;
}

/**
 * The QR code for `text` as an SVG viewBox size and dark-module path. Error correction M (15%) keeps an invite-sized
 * code small enough to scan from a screen. Throws when the text does not fit in a QR code.
 */
export function qrSvg(text: string, margin = QR_MARGIN): QrSvg {
  const qr = create(text, { errorCorrectionLevel: "M" });
  return { size: qr.modules.size + margin * 2, path: qrPath(qr.modules, margin), version: qr.version };
}
