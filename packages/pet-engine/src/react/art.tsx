// Built-in SVG characters (first-class fallback when no sprite sheet is available).
// All characters face RIGHT in a 128×128 box with feet on y≈114. Parts carry `ap-*` classes that the
// stylesheet (styles.ts) animates per activity. Never put a `transform` attribute on an animated
// element (CSS transforms would override it) — position static wrappers instead.
import type { JSX } from "react";
import type { Species } from "@krypto/protocol";

// ---------------------------------------------------------------- shared FX pieces

/** Bed / nest / charging pad, drawn behind the character, visible while napping. */
function Bed({ kind }: { kind: "cushion" | "nest" | "pad" }): JSX.Element {
  if (kind === "nest") {
    return (
      <g className="ap-fx-bed">
        <ellipse cx="62" cy="110" rx="34" ry="9" className="ap-f-bed2 ap-s" />
        <path d="M30 106 q8 -6 16 0 q8 -6 16 0 q8 -6 16 0 q8 -6 16 0" className="ap-k-bed" />
        <path d="M32 112 q8 4 16 0 q8 4 16 0 q8 4 16 0 q8 4 14 0" className="ap-k-bed" />
      </g>
    );
  }
  if (kind === "pad") {
    return (
      <g className="ap-fx-bed">
        <rect x="26" y="108" width="76" height="8" rx="4" className="ap-f-bed2 ap-s" />
        <rect x="34" y="110" width="60" height="3" rx="1.5" className="ap-f-glow ap-pad-glow" />
      </g>
    );
  }
  return (
    <g className="ap-fx-bed">
      <ellipse cx="64" cy="111" rx="48" ry="10" className="ap-f-bed2 ap-s" />
      <ellipse cx="64" cy="108" rx="40" ry="7" className="ap-f-bed" />
    </g>
  );
}

/** Effects floating above the head. (x, y) = top of the head. */
function OverheadFx({ x, y, sound }: { x: number; y: number; sound: string }): JSX.Element {
  return (
    <g transform={`translate(${x} ${y})`}>
      <g className="ap-fx-zzz">
        <g className="ap-unflip">
          <text x="10" y="-4" className="ap-z ap-z1">z</text>
          <text x="18" y="-13" className="ap-z ap-z2">z</text>
          <text x="27" y="-23" className="ap-z ap-z3">Z</text>
        </g>
      </g>
      <g className="ap-fx-think">
        <circle cx="9" cy="-3" r="2" className="ap-f-white ap-s ap-thin" />
        <circle cx="14" cy="-9" r="3" className="ap-f-white ap-s ap-thin" />
        <g className="ap-think-cloud">
          <ellipse cx="25" cy="-21" rx="11.5" ry="9" className="ap-f-white ap-s ap-thin" />
          <g className="ap-unflip">
            <text x="25" y="-16.5" className="ap-think-q">?</text>
          </g>
        </g>
      </g>
      <g className="ap-fx-alert">
        <g className="ap-bang">
          <path d="M-14 -32 L -8 -32 L -9.5 -14 L -12.5 -14 Z" className="ap-f-bang ap-s ap-thin" />
          <circle cx="-11" cy="-8.5" r="3" className="ap-f-bang ap-s ap-thin" />
        </g>
      </g>
      <g className="ap-fx-happy">
        <path className="ap-heart ap-heart1 ap-f-heart" d="M8 -6 c -3 -4 -9 -1 -6 3 l 6 5 l 6 -5 c 3 -4 -3 -7 -6 -3 z" />
        <path className="ap-heart ap-heart2 ap-f-heart" d="M24 -2 c -2.4 -3.2 -7.2 -0.8 -4.8 2.4 l 4.8 4 l 4.8 -4 c 2.4 -3.2 -2.4 -5.6 -4.8 -2.4 z" />
        <path className="ap-sparkle ap-sparkle1 ap-f-spark" d="M-8 -10 l 1.5 4 l 4 1.5 l -4 1.5 l -1.5 4 l -1.5 -4 l -4 -1.5 l 4 -1.5 z" />
        <path className="ap-sparkle ap-sparkle2 ap-f-spark" d="M32 -18 l 1.2 3 l 3 1.2 l -3 1.2 l -1.2 3 l -1.2 -3 l -3 -1.2 l 3 -1.2 z" />
      </g>
      <g className="ap-fx-sad">
        <g className="ap-rain-cloud">
          <path
            d="M-6 -14 a 6 6 0 0 1 4 -10 a 8 8 0 0 1 15 -1 a 6 6 0 0 1 3 11 z"
            className="ap-f-cloud ap-s ap-thin"
          />
          <line x1="-2" y1="-11" x2="-3" y2="-7" className="ap-rain ap-rain1" />
          <line x1="5" y1="-11" x2="4" y2="-7" className="ap-rain ap-rain2" />
          <line x1="12" y1="-11" x2="11" y2="-7" className="ap-rain ap-rain3" />
        </g>
      </g>
      {/* species sound shown while alerting, kept out of the head so it never rotates */}
      <g className="ap-fx-sound">
        <g className="ap-unflip">
          <text x="14" y="-12" className="ap-sound">
            {sound}
          </text>
        </g>
      </g>
    </g>
  );
}

/** Bark/voice lines out of the mouth. (x, y) = mouth corner. */
function BarkFx({ x, y }: { x: number; y: number }): JSX.Element {
  return (
    <g transform={`translate(${x} ${y})`}>
      <g className="ap-fx-bark">
        <g className="ap-bark-lines">
          <line x1="4" y1="-4" x2="11" y2="-9" />
          <line x1="5" y1="1" x2="13" y2="1" />
          <line x1="4" y1="6" x2="11" y2="11" />
        </g>
      </g>
    </g>
  );
}

/** Checklist held at (x, y) (top-center of the clipboard). */
function ClipboardFx({ x, y }: { x: number; y: number }): JSX.Element {
  return (
    <g transform={`translate(${x} ${y})`}>
      <g className="ap-fx-plan">
        <rect x="-10" y="0" width="20" height="25" rx="2.5" className="ap-f-clip ap-s ap-thin" />
        <rect x="-8" y="3" width="16" height="20" rx="1.2" className="ap-f-paper" />
        <rect x="-4" y="-2" width="8" height="4" rx="1.5" className="ap-f-metal2 ap-s ap-thin" />
        <path d="M-6 8 h3 M-6 13.5 h3 M-6 19 h3" className="ap-k-box" />
        <path d="M0 8 h6 M0 13.5 h6 M0 19 h5" className="ap-k-text" />
        <path d="M-6.5 7.5 l 1.5 1.6 l 3 -3.2" className="ap-check ap-check1" />
        <path d="M-6.5 13 l 1.5 1.6 l 3 -3.2" className="ap-check ap-check2" />
        <path d="M-6.5 18.5 l 1.5 1.6 l 3 -3.2" className="ap-check ap-check3" />
      </g>
    </g>
  );
}

/** Tear below the eye at (x, y). Put it inside the head group so it follows head poses. */
function TearFx({ x, y }: { x: number; y: number }): JSX.Element {
  return (
    <g transform={`translate(${x} ${y})`}>
      <g className="ap-fx-tear">
        <path className="ap-tear ap-f-tear" d="M0 0 c 2 3 3 5 3 6.5 a 3 3 0 0 1 -6 0 c 0 -1.5 1 -3.5 3 -6.5 z" />
      </g>
    </g>
  );
}

/** Dirt (or sparks) flung backwards from the paws at (x, y). */
function DigFx({ x, y }: { x: number; y: number }): JSX.Element {
  return (
    <g transform={`translate(${x} ${y})`}>
      <g className="ap-fx-dig">
        <ellipse cx="6" cy="2" rx="14" ry="4.5" className="ap-f-dirt" />
        <circle cx="0" cy="-2" r="2.6" className="ap-dirt ap-dirt1 ap-f-dirt" />
        <circle cx="4" cy="-3" r="2" className="ap-dirt ap-dirt2 ap-f-dirt" />
        <circle cx="-2" cy="-1" r="1.8" className="ap-dirt ap-dirt3 ap-f-dirt" />
        <rect x="2" y="-3" width="3" height="3" rx="0.8" className="ap-dirt ap-dirt4 ap-f-dirt" />
      </g>
    </g>
  );
}

// ---------------------------------------------------------------- dog (the hero)

function DogSvg(): JSX.Element {
  return (
    <svg viewBox="0 0 128 128" className="ap-svg" aria-hidden="true" focusable="false">
      <Bed kind="cushion" />
      <g className="ap-char">
        <g className="ap-tail">
          <path d="M35 74 C 24 71, 16 62, 15 50 C 14.6 45, 20.6 44.4, 21.6 49.4 C 23 57.5, 28.5 63.5, 38 66.5 Z" className="ap-f-fur ap-s" />
        </g>
        <g className="ap-leg ap-leg-b1">
          <rect x="37" y="84" width="10" height="27" rx="5" className="ap-f-fur2" />
          <ellipse cx="43" cy="111.5" rx="7" ry="3.8" className="ap-f-fur2" />
        </g>
        <g className="ap-leg ap-leg-f1">
          <rect x="79" y="84" width="10" height="27" rx="5" className="ap-f-fur2" />
          <ellipse cx="85" cy="111.5" rx="7" ry="3.8" className="ap-f-fur2" />
        </g>
        <g className="ap-body">
          <ellipse cx="62" cy="80" rx="31" ry="18" className="ap-f-fur ap-s" />
          <ellipse cx="66" cy="89.5" rx="20" ry="6.5" className="ap-f-belly" />
          <ellipse cx="49" cy="72" rx="10" ry="6.5" className="ap-f-patch" />
        </g>
        <g className="ap-leg ap-leg-b2">
          <rect x="45" y="86" width="11" height="26" rx="5.5" className="ap-f-fur ap-s" />
          <ellipse cx="52" cy="112.5" rx="8" ry="4" className="ap-f-fur ap-s" />
        </g>
        <g className="ap-leg ap-leg-f2">
          <rect x="87" y="86" width="11" height="26" rx="5.5" className="ap-f-fur ap-s" />
          <ellipse cx="94" cy="112.5" rx="8" ry="4" className="ap-f-fur ap-s" />
        </g>
        <g className="ap-collar">
          <path d="M75.5 63 Q 80.5 73 78 84.5" className="ap-k-collar" />
          <circle cx="78.8" cy="87" r="3.3" className="ap-f-tag ap-s ap-thin" />
        </g>
        <g className="ap-head">
          <path className="ap-ear ap-ear-far ap-f-ear2" d="M95 33 C 103 30, 108 39, 106.5 49 C 105.8 54, 100.8 54, 99.6 50 C 98.4 44, 97.4 38, 95 33 Z" />
          <circle cx="90" cy="50" r="18" className="ap-f-fur ap-s" />
          <ellipse cx="105" cy="58" rx="13" ry="9.5" className="ap-f-belly ap-s" />
          <ellipse cx="116.2" cy="53.6" rx="4.8" ry="3.8" className="ap-f-nose" />
          <ellipse cx="115" cy="52.4" rx="1.5" ry="0.9" className="ap-f-white ap-shine" />
          <path className="ap-mouth-closed ap-k-line" d="M106 63 Q 110 66.5 114.5 62" />
          <path className="ap-mouth-open ap-f-mouth ap-s ap-thin" d="M104 61.5 Q 110.5 73 117.5 60.5 Z" />
          <path className="ap-tongue ap-f-tongue ap-s ap-thin" d="M107.5 64.5 Q 108.5 76 113 74 Q 116.5 72 114.5 63.5 Z" />
          <g className="ap-eye-open">
            <circle cx="97" cy="45" r="3.4" className="ap-f-eye" />
            <circle cx="98.2" cy="43.8" r="1.15" className="ap-f-white" />
          </g>
          <path className="ap-eye-closed ap-k-eye" d="M93.5 46 Q 97 48.8 100.5 46" />
          <path className="ap-eye-happy ap-k-eye" d="M93.5 47.2 Q 97 41.8 100.5 47.2" />
          <path className="ap-brow ap-k-line" d="M93 41.5 L 100.5 38.6" />
          <ellipse className="ap-cheek ap-f-cheek" cx="100" cy="54" rx="3.2" ry="2" />
          <path className="ap-ear ap-ear-near ap-f-ear ap-s" d="M83.5 33 C 74.5 33, 70.5 44, 72.5 57 C 73.5 64, 80.5 65, 83.2 59 C 86.2 52, 88.4 42, 83.5 33 Z" />
          <TearFx x={98} y={50} />
          <BarkFx x={117} y={61} />
          <ClipboardFx x={110} y={64} />
        </g>
      </g>
      <OverheadFx x={92} y={30} sound="WOOF!" />
      <DigFx x={96} y={113} />
    </svg>
  );
}

// ---------------------------------------------------------------- cat

function CatSvg(): JSX.Element {
  return (
    <svg viewBox="0 0 128 128" className="ap-svg" aria-hidden="true" focusable="false">
      <Bed kind="cushion" />
      <g className="ap-char">
        <g className="ap-tail">
          <path d="M38 82 C 20 80, 11 66, 16 52 C 18.5 45, 26 45.5, 25 51.5 C 21.5 61, 25 72, 39 75 Z" className="ap-f-fur ap-s" />
          <path d="M17.5 55 q 4 1 7 -1 M17 62 q 4 1 7 -1.5" className="ap-k-stripe" />
        </g>
        <g className="ap-leg ap-leg-b1">
          <rect x="40" y="86" width="8" height="25" rx="4" className="ap-f-fur2" />
          <ellipse cx="45" cy="111.5" rx="5.6" ry="3.2" className="ap-f-fur2" />
        </g>
        <g className="ap-leg ap-leg-f1">
          <rect x="78" y="86" width="8" height="25" rx="4" className="ap-f-fur2" />
          <ellipse cx="83" cy="111.5" rx="5.6" ry="3.2" className="ap-f-fur2" />
        </g>
        <g className="ap-body">
          <ellipse cx="62" cy="84" rx="28" ry="14" className="ap-f-fur ap-s" />
          <ellipse cx="65" cy="91" rx="17" ry="5" className="ap-f-belly" />
          <path d="M50 71 q 3 6 0 12 M58 70 q 3 6 0 12 M66 70.5 q 3 6 0 11" className="ap-k-stripe" />
        </g>
        <g className="ap-leg ap-leg-b2">
          <rect x="47" y="88" width="9" height="24" rx="4.5" className="ap-f-fur ap-s" />
          <ellipse cx="52.5" cy="112.5" rx="6.4" ry="3.4" className="ap-f-fur ap-s" />
        </g>
        <g className="ap-leg ap-leg-f2">
          <rect x="84" y="88" width="9" height="24" rx="4.5" className="ap-f-fur ap-s" />
          <ellipse cx="89.5" cy="112.5" rx="6.4" ry="3.4" className="ap-f-fur ap-s" />
        </g>
        <g className="ap-collar">
          <path d="M78 66 Q 82 74 80 82" className="ap-k-collar ap-k-collar-thin" />
          <circle cx="80.6" cy="84" r="2.4" className="ap-f-tag ap-s ap-thin" />
        </g>
        <g className="ap-head">
          <path className="ap-ear ap-ear-far ap-f-fur2 ap-s" d="M93 44 L 101 29 L 104.5 47 Z" />
          <circle cx="90" cy="56" r="15.5" className="ap-f-fur ap-s" />
          <g className="ap-ear ap-ear-near">
            <path className="ap-f-fur ap-s" d="M76.5 48 L 79.5 30.5 L 91.5 42.5 Z" />
            <path d="M79.6 44.5 L 81 35 L 87.5 41.6 Z" className="ap-f-inner" />
          </g>
          <path d="M84 44 q 2 4 0 7 M89 42.5 q 1.6 3.4 0 6.4" className="ap-k-stripe" />
          <ellipse cx="101" cy="61.5" rx="7" ry="5" className="ap-f-belly" />
          <path d="M103.4 57.2 L 107.4 57.2 L 105.4 60 Z" className="ap-f-nose" />
          <path className="ap-mouth-closed ap-k-line ap-thin" d="M101.5 62.5 q 2 2.2 4 0 q 2 2.2 4 0" />
          <ellipse className="ap-mouth-open ap-f-mouth ap-s ap-thin" cx="105" cy="64.5" rx="3.4" ry="3.2" />
          <path className="ap-tongue ap-f-tongue" d="M103.2 65 q 1.8 4.6 3.6 0 z" />
          <g className="ap-eye-open">
            <ellipse cx="96" cy="52.5" rx="3" ry="3.6" className="ap-f-iris" />
            <ellipse cx="96.6" cy="52.5" rx="1" ry="2.9" className="ap-f-eye" />
            <circle cx="97.2" cy="51.2" r="0.8" className="ap-f-white" />
          </g>
          <path className="ap-eye-closed ap-k-eye" d="M92.8 53 Q 96 55.6 99.2 53" />
          <path className="ap-eye-happy ap-k-eye" d="M92.8 54 Q 96 49 99.2 54" />
          <path className="ap-brow ap-k-line ap-thin" d="M92.5 47.8 L 99.5 45.6" />
          <ellipse className="ap-cheek ap-f-cheek" cx="98.5" cy="60" rx="2.6" ry="1.7" />
          <path d="M106 61.5 L 119 58.5 M106 63.5 L 119 64 M105.5 65.5 L 117.5 68.5" className="ap-k-whisker" />
          <TearFx x={96.5} y={57} />
          <BarkFx x={109} y={64} />
          <ClipboardFx x={105} y={67} />
        </g>
      </g>
      <OverheadFx x={90} y={36} sound="MEOW!" />
      <DigFx x={92} y={113} />
    </svg>
  );
}

// ---------------------------------------------------------------- bird

function BirdSvg(): JSX.Element {
  return (
    <svg viewBox="0 0 128 128" className="ap-svg" aria-hidden="true" focusable="false">
      <Bed kind="nest" />
      <g className="ap-char">
        <g className="ap-tail">
          <path d="M44 78 L 18 66 L 24 76 L 15 81 L 26 84 L 20 92 L 45 87 Z" className="ap-f-wing ap-s" />
        </g>
        <g className="ap-leg ap-leg-b1">
          <path d="M56 96 L 54 112 M54 112 l -6 1.5 M54 112 l 1 2.5 M54 112 l 5 1" className="ap-k-leg" />
        </g>
        <g className="ap-body">
          <ellipse cx="62" cy="80" rx="25" ry="20" className="ap-f-fur ap-s" />
          <ellipse cx="68" cy="88" rx="15" ry="10" className="ap-f-belly" />
        </g>
        <g className="ap-leg ap-leg-f1">
          <path d="M68 97 L 70 112 M70 112 l -5 1.5 M70 112 l 1.5 2.5 M70 112 l 6 1" className="ap-k-leg" />
        </g>
        <g className="ap-wing">
          <path d="M46 70 C 55 63, 71 66, 74 78 C 71 89, 57 92, 45 85 C 41 80, 42 74, 46 70 Z" className="ap-f-wing ap-s" />
          <path d="M50 78 q 8 4 18 2 M50 84 q 7 3 14 1" className="ap-k-feather" />
        </g>
        <g className="ap-head">
          <path className="ap-ear ap-f-crest ap-s ap-thin" d="M76 38 C 72 29, 78 26, 81 34 C 82 25, 89 26, 86.5 36 C 90 31, 94 34, 89 39 Z" />
          <circle cx="83" cy="49" r="14.5" className="ap-f-fur ap-s" />
          <g className="ap-mouth-closed">
            <path d="M95 45.5 L 108 50.5 L 95 55.5 Z" className="ap-f-beak ap-s ap-thin" />
            <path d="M95.5 50.5 L 106 50.5" className="ap-k-line ap-thin" />
          </g>
          <g className="ap-mouth-open">
            <path d="M95 44.5 L 108 48.5 L 95.5 50 Z" className="ap-f-beak ap-s ap-thin" />
            <path d="M95.5 52.5 L 105 56.5 L 95 57 Z" className="ap-f-beak ap-s ap-thin" />
            <path d="M95.5 50 L 104 49.6 L 104 55.6 L 95.5 52.5 Z" className="ap-f-mouth" />
          </g>
          <g className="ap-eye-open">
            <circle cx="89" cy="46" r="3" className="ap-f-eye" />
            <circle cx="90" cy="45" r="1" className="ap-f-white" />
          </g>
          <path className="ap-eye-closed ap-k-eye" d="M86 47 Q 89 49.5 92 47" />
          <path className="ap-eye-happy ap-k-eye" d="M86 48 Q 89 43 92 48" />
          <path className="ap-brow ap-k-line ap-thin" d="M85.5 41.6 L 92 39.4" />
          <circle className="ap-cheek ap-f-cheek" cx="89" cy="54" r="2.8" />
          <TearFx x={88.5} y={50} />
          <BarkFx x={107} y={51} />
          <ClipboardFx x={101} y={55} />
        </g>
      </g>
      <OverheadFx x={83} y={30} sound="TWEET!" />
      <DigFx x={86} y={113} />
    </svg>
  );
}

// ---------------------------------------------------------------- robot

function RobotSvg(): JSX.Element {
  return (
    <svg viewBox="0 0 128 128" className="ap-svg" aria-hidden="true" focusable="false">
      <Bed kind="pad" />
      <g className="ap-char">
        <g className="ap-leg ap-leg-b1">
          <rect x="52" y="94" width="10" height="16" rx="3" className="ap-f-metal2 ap-s" />
          <rect x="49" y="108" width="17" height="6" rx="3" className="ap-f-metal2 ap-s" />
        </g>
        <g className="ap-leg ap-leg-f1">
          <rect x="72" y="94" width="10" height="16" rx="3" className="ap-f-metal ap-s" />
          <rect x="70" y="108" width="18" height="6" rx="3" className="ap-f-metal ap-s" />
        </g>
        <g className="ap-body">
          <rect x="45" y="60" width="46" height="37" rx="9" className="ap-f-metal ap-s" />
          <rect x="53" y="67" width="22" height="15" rx="3.5" className="ap-f-screen" />
          <circle cx="58.5" cy="74.5" r="2.4" className="ap-f-light ap-chest ap-chest1" />
          <circle cx="64.5" cy="74.5" r="2.4" className="ap-f-glow ap-chest ap-chest2" />
          <circle cx="70.5" cy="74.5" r="2.4" className="ap-f-spark ap-chest ap-chest3" />
          <path d="M52 89 h 16 M52 92.5 h 10" className="ap-k-panel" />
        </g>
        <g className="ap-head">
          <g className="ap-ear">
            <path d="M80 27 L 80 14" className="ap-k-line" />
            <circle cx="80" cy="11.5" r="3.6" className="ap-f-light ap-light ap-s ap-thin" />
          </g>
          <rect x="74" y="52" width="10" height="9" rx="2" className="ap-f-metal2 ap-s" />
          <rect x="63" y="26" width="36" height="28" rx="7" className="ap-f-metal ap-s" />
          <rect x="67" y="30" width="28" height="20" rx="4.5" className="ap-f-screen" />
          <rect x="61" y="35" width="3" height="10" rx="1.5" className="ap-f-metal2" />
          <g className="ap-eye-open">
            <rect x="80" y="35" width="4.4" height="7" rx="2" className="ap-f-glow" />
            <rect x="88" y="35" width="4.4" height="7" rx="2" className="ap-f-glow" />
          </g>
          <path className="ap-eye-closed ap-k-glow" d="M79.5 40 h 5.4 M87.5 40 h 5.4" />
          <path className="ap-eye-happy ap-k-glow" d="M79.6 41 l 2.6 -3.4 l 2.6 3.4 M87.6 41 l 2.6 -3.4 l 2.6 3.4" />
          <path className="ap-brow ap-k-glow" d="M79.5 33.5 l 5 -1.6 M87.5 31.9 l 5 1.6" />
          <path className="ap-mouth-closed ap-k-glow" d="M83 46 h 7" />
          <rect className="ap-mouth-open ap-f-glow" x="83" y="44.2" width="7" height="4" rx="1.6" />
          <path className="ap-tongue ap-k-glow" d="M82.5 45.4 q 4 3 8 0" />
          <circle className="ap-cheek ap-f-cheek" cx="93" cy="45.5" r="1.8" />
          <TearFx x={90} y={43} />
          <BarkFx x={98} y={45} />
        </g>
        <g className="ap-arm">
          <path d="M84 67 L 99 79" className="ap-k-arm" />
          <circle cx="100.5" cy="80.5" r="4.4" className="ap-f-metal2 ap-s" />
          <ClipboardFx x={104} y={78} />
        </g>
      </g>
      <OverheadFx x={83} y={22} sound="BEEP!" />
      <DigFx x={100} y={112} />
    </svg>
  );
}

export function SpeciesSvg({ species }: { species: Species }): JSX.Element {
  switch (species) {
    case "cat":
      return <CatSvg />;
    case "bird":
      return <BirdSvg />;
    case "robot":
      return <RobotSvg />;
    case "dog":
    default:
      return <DogSvg />;
  }
}
