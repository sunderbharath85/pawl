// The name rule peers enforce on device and machine names (crates/peer intro.rs `device_name_ok`; vectors in
// crates/peer/tests/vectors/names.json). Peers refuse frames that carry a name breaking it, so clients pre-check
// names here before sending them. Pure, no I/O. Code points are listed as numbers so no editor or tool can turn an
// escape into the invisible character itself.

/** Max Unicode code points of a trimmed device / machine name peers accept (MAX_INTRO_NAME_CHARS). */
export const DEVICE_NAME_MAX = 64;

/** Unicode White_Space (what Rust's `str::trim` removes). Not JS `trim()`: that also removes U+FEFF and keeps U+0085. */
const WHITE_SPACE: ReadonlySet<number> = new Set([
  0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x20, 0x85, 0xa0, 0x1680, 0x2000, 0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007,
  0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000,
]);

/** Control characters (Cc) and the invisible format characters peers refuse anywhere in a name. */
const FORBIDDEN: ReadonlyArray<readonly [number, number]> = [
  [0x0000, 0x001f], // C0 controls
  [0x007f, 0x009f], // DEL + C1 controls
  [0x061c, 0x061c], // Arabic letter mark
  [0x200b, 0x200f], // zero-width space / joiners, LRM, RLM
  [0x2028, 0x202e], // line / paragraph separators, bidi embeddings and overrides
  [0x2060, 0x2064], // word joiner, invisible operators
  [0x2066, 0x2069], // bidi isolates
  [0xfeff, 0xfeff], // zero-width no-break space (BOM)
];

function isWhiteSpace(cp: number): boolean {
  return WHITE_SPACE.has(cp);
}

/** A forbidden character, or a lone UTF-16 surrogate (not a Unicode scalar value, so not valid UTF-8 either). */
function isForbidden(cp: number): boolean {
  if (cp >= 0xd800 && cp <= 0xdfff) return true;
  return FORBIDDEN.some(([lo, hi]) => cp >= lo && cp <= hi);
}

/** `s` with Unicode White_Space removed from both ends (exactly what peers trim). */
export function trimWhiteSpace(s: string): string {
  const chars = [...s];
  let a = 0;
  let b = chars.length;
  while (a < b && isWhiteSpace(chars[a]!.codePointAt(0)!)) a++;
  while (b > a && isWhiteSpace(chars[b - 1]!.codePointAt(0)!)) b--;
  return chars.slice(a, b).join("");
}

export type DeviceNameProblem = "empty" | "too_long" | "bad_char";

/**
 * Why peers would refuse `name`, or null when they accept it: a control or invisible format character anywhere in
 * the untrimmed name (`bad_char`), then 1..=`max` code points once trimmed (`empty` / `too_long`).
 */
export function deviceNameProblem(name: string, max: number = DEVICE_NAME_MAX): DeviceNameProblem | null {
  for (const ch of name) if (isForbidden(ch.codePointAt(0)!)) return "bad_char";
  const length = [...trimWhiteSpace(name)].length;
  if (length === 0) return "empty";
  return length > max ? "too_long" : null;
}

/** True when peers accept `name` (crates/peer `device_name_ok`). */
export function deviceNameOk(name: string): boolean {
  return deviceNameProblem(name) === null;
}
