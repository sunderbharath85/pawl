// Stylesheet for PetSprite / PetBubble / PetStage, injected once as a <style> tag.
// Exported as PET_CSS so hosts with strict CSP can ship it as a file instead (see configurePetStyles).

const A = (activity: string) => `.ap-pet[data-activity="${activity}"]`;
const SP = (species: string, activity?: string) =>
  activity ? `.ap-pet.ap-sp-${species}[data-activity="${activity}"]` : `.ap-pet.ap-sp-${species}`;
const W = `.ap-pet[data-walking="true"]`;

export const PET_CSS = `
/* ---------- layout ---------- */
.ap-pet{position:relative;display:inline-block;flex:none;line-height:0;vertical-align:bottom;-webkit-user-select:none;user-select:none}
.ap-zoom,.ap-zoom-flip,.ap-flip,.ap-move{position:absolute;inset:0}
.ap-pet[data-facing="left"] .ap-flip{transform:scaleX(-1)}
.ap-svg{width:100%;height:100%;display:block;overflow:visible}
.ap-strip{width:100%;height:100%;background-repeat:no-repeat;background-position:0 0}
@keyframes ap-strip{from{background-position:0 0}to{background-position:var(--ap-strip-end,0) 0}}
.ap-shadow{position:absolute;left:22%;right:22%;bottom:4.5%;height:6%;border-radius:50%;background:radial-gradient(closest-side,rgba(0,0,0,.24),rgba(0,0,0,0))}
.ap-has-strip .ap-shadow{display:none}
.ap-has-strip .ap-zoom,.ap-has-strip .ap-zoom-flip,.ap-has-strip .ap-move{animation:none!important}

/* ---------- palettes (species × skin) ---------- */
.ap-pet{--ap-line:#5a3a1e;--ap-eye:#2b1d14;--ap-white:#fff;--ap-mouth:#7a2230;--ap-tongue:#ff7d94;--ap-cheek:#ff8fa3;--ap-collar:#e0483e;--ap-tag:#ffd54a;--ap-bed:#f2a7a0;--ap-bed2:#c96a64;--ap-dirt:#8b5a2b;--ap-glow:#4ff0ff;--ap-light:#ff5a5a;--ap-metal:#c9d3de;--ap-metal2:#8d9aa8;--ap-screen:#1f2a37}
${SP("dog")}{--ap-fur:#d9a066;--ap-fur2:#b9824a;--ap-belly:#f6dcb4;--ap-patch:#c48a50;--ap-ear:#8a5426;--ap-ear2:#6e4220;--ap-nose:#2b1d14}
${SP("dog")}[data-skin="golden"]{--ap-fur:#f0b955;--ap-fur2:#d3973a;--ap-belly:#fde6b0;--ap-patch:#e3a640;--ap-ear:#c98a2c;--ap-ear2:#a66f1f}
${SP("dog")}[data-skin="black"]{--ap-fur:#3b3735;--ap-fur2:#2a2726;--ap-belly:#c9a27a;--ap-patch:#4a4542;--ap-ear:#262322;--ap-ear2:#1b1918;--ap-line:#141212;--ap-nose:#0d0c0c}
${SP("dog")}[data-skin="white"]{--ap-fur:#f7f3ee;--ap-fur2:#e2dbd2;--ap-belly:#ffffff;--ap-patch:#e8ddd0;--ap-ear:#d9c8b4;--ap-ear2:#c4b29d;--ap-line:#7d6a58}
${SP("dog")}[data-skin="choco"]{--ap-fur:#7b4a2a;--ap-fur2:#633a20;--ap-belly:#b98260;--ap-patch:#6b3f23;--ap-ear:#4e2c16;--ap-ear2:#3e2311;--ap-line:#2e1a0c}
${SP("cat")}{--ap-fur:#a3abb5;--ap-fur2:#848c97;--ap-belly:#e9edf1;--ap-inner:#f5b3c0;--ap-nose:#f08aa0;--ap-iris:#8fd16a;--ap-stripe:#6e7681;--ap-line:#3f4650;--ap-collar:#3f8cff}
${SP("cat")}[data-skin="orange"]{--ap-fur:#f2a154;--ap-fur2:#d9853a;--ap-belly:#fde3c4;--ap-stripe:#c8722c;--ap-line:#6b3a12}
${SP("cat")}[data-skin="black"]{--ap-fur:#34343c;--ap-fur2:#24242a;--ap-belly:#4a4a54;--ap-stripe:#2a2a31;--ap-iris:#f5d33c;--ap-line:#111114}
${SP("cat")}[data-skin="white"]{--ap-fur:#f8f6f2;--ap-fur2:#e5e0d8;--ap-belly:#fff;--ap-stripe:#e9e2d8;--ap-iris:#6cb8f0;--ap-line:#857a6d}
${SP("bird")}{--ap-fur:#5bb6f0;--ap-belly:#fff1b8;--ap-wing:#3a8fd0;--ap-crest:#ff7a59;--ap-beak:#ffb02e;--ap-leg:#e8902a;--ap-line:#1f4f7a;--ap-bed:#d8b077;--ap-bed2:#a87b45}
${SP("bird")}[data-skin="yellow"]{--ap-fur:#ffd84a;--ap-belly:#fff4c4;--ap-wing:#f0b62a;--ap-crest:#ff9a3c;--ap-line:#7a5a10}
${SP("bird")}[data-skin="green"]{--ap-fur:#5ccf7a;--ap-belly:#e8f7b0;--ap-wing:#2f9e57;--ap-crest:#ff5a5a;--ap-line:#1d5c34}
${SP("bird")}[data-skin="pink"]{--ap-fur:#ff9cc6;--ap-belly:#ffe1ef;--ap-wing:#e86fa6;--ap-crest:#9b6cff;--ap-line:#7a2e52}
${SP("robot")}{--ap-line:#3d4652;--ap-bed:#3d4652;--ap-bed2:#2a313a;--ap-dirt:#ffd54a;--ap-cheek:#ff8fa3}
${SP("robot")}[data-skin="gold"]{--ap-metal:#f0cf6a;--ap-metal2:#c9a440;--ap-line:#6b5414}
${SP("robot")}[data-skin="blue"]{--ap-metal:#8fc3ff;--ap-metal2:#5a93d6;--ap-line:#1f3f6b}
${SP("robot")}[data-skin="retro"]{--ap-metal:#e8d9b5;--ap-metal2:#c9b487;--ap-glow:#9bff6a;--ap-line:#5a4a2a}

/* ---------- paint ---------- */
.ap-s{stroke:var(--ap-line);stroke-width:2;stroke-linejoin:round;stroke-linecap:round}
.ap-thin{stroke-width:1.3}
.ap-f-fur{fill:var(--ap-fur)}.ap-f-fur2{fill:var(--ap-fur2)}.ap-f-belly{fill:var(--ap-belly)}.ap-f-patch{fill:var(--ap-patch)}
.ap-f-ear{fill:var(--ap-ear)}.ap-f-ear2{fill:var(--ap-ear2)}.ap-f-nose{fill:var(--ap-nose)}.ap-f-white{fill:var(--ap-white)}
.ap-f-mouth{fill:var(--ap-mouth)}.ap-f-tongue{fill:var(--ap-tongue)}.ap-f-eye{fill:var(--ap-eye)}.ap-f-iris{fill:var(--ap-iris)}
.ap-f-cheek{fill:var(--ap-cheek);opacity:.55}.ap-f-tag{fill:var(--ap-tag)}.ap-f-bed{fill:var(--ap-bed)}.ap-f-bed2{fill:var(--ap-bed2)}
.ap-f-glow{fill:var(--ap-glow)}.ap-f-light{fill:var(--ap-light)}.ap-f-metal{fill:var(--ap-metal)}.ap-f-metal2{fill:var(--ap-metal2)}
.ap-f-screen{fill:var(--ap-screen)}.ap-f-inner{fill:var(--ap-inner)}.ap-f-wing{fill:var(--ap-wing)}.ap-f-crest{fill:var(--ap-crest)}
.ap-f-beak{fill:var(--ap-beak)}.ap-f-dirt{fill:var(--ap-dirt)}.ap-f-bang{fill:#ff4d3d}.ap-f-heart{fill:#ff5c8a}.ap-f-spark{fill:#ffd54a}
.ap-f-cloud{fill:#b8c2cf}.ap-f-tear{fill:#6ec6ff}.ap-f-clip{fill:#b07a45}.ap-f-paper{fill:#fffdf6}
.ap-shine{opacity:.6}
.ap-k-line{fill:none;stroke:var(--ap-line);stroke-width:2;stroke-linecap:round;stroke-linejoin:round}
.ap-k-eye{fill:none;stroke:var(--ap-eye);stroke-width:2;stroke-linecap:round}
.ap-k-collar{fill:none;stroke:var(--ap-collar);stroke-width:6;stroke-linecap:round}
.ap-k-collar-thin{stroke-width:4}
.ap-k-stripe{fill:none;stroke:var(--ap-stripe);stroke-width:2;stroke-linecap:round}
.ap-k-whisker{fill:none;stroke:var(--ap-line);stroke-width:.9;stroke-linecap:round;opacity:.75}
.ap-k-leg{fill:none;stroke:var(--ap-leg);stroke-width:3;stroke-linecap:round;stroke-linejoin:round}
.ap-k-feather{fill:none;stroke:var(--ap-line);stroke-width:1.2;stroke-linecap:round;opacity:.45}
.ap-k-glow{fill:none;stroke:var(--ap-glow);stroke-width:2;stroke-linecap:round;stroke-linejoin:round}
.ap-k-panel{fill:none;stroke:var(--ap-metal2);stroke-width:2;stroke-linecap:round}
.ap-k-arm{fill:none;stroke:var(--ap-metal2);stroke-width:5;stroke-linecap:round}
.ap-k-bed{fill:none;stroke:var(--ap-bed2);stroke-width:2;stroke-linecap:round;opacity:.8}
.ap-k-box{fill:none;stroke:#b9b2a3;stroke-width:1.2;stroke-linecap:round}
.ap-k-text{fill:none;stroke:#8c8577;stroke-width:1.2;stroke-linecap:round}
.ap-check{fill:none;stroke:#2fa84f;stroke-width:1.6;stroke-linecap:round;stroke-linejoin:round;stroke-dasharray:12;stroke-dashoffset:0}
.ap-bark-lines line{stroke:var(--ap-line);stroke-width:2;stroke-linecap:round}
.ap-rain{stroke:#6ec6ff;stroke-width:1.6;stroke-linecap:round}
.ap-z{font:800 11px/1 ui-rounded,system-ui,-apple-system,"Segoe UI",sans-serif;fill:#7b8cff}
.ap-z2{font-size:13px}.ap-z3{font-size:16px}
.ap-think-q{font:900 12px/1 ui-rounded,system-ui,-apple-system,"Segoe UI",sans-serif;fill:#5a6b85;text-anchor:middle}
.ap-sound{font:900 10px/1 ui-rounded,system-ui,-apple-system,"Segoe UI",sans-serif;fill:#ff4d3d;stroke:#fff;stroke-width:2.5px;paint-order:stroke;letter-spacing:.3px;text-anchor:middle}

/* ---------- transform origins (SVG boxes) ---------- */
.ap-svg *{transform-box:fill-box}
.ap-char{transform-origin:55% 100%;transition:transform .35s ease}
.ap-body{transform-origin:50% 100%}
.ap-head{transform-origin:20% 80%;transition:transform .3s ease}
.ap-ear{transform-origin:50% 4%;transition:transform .3s ease}
.ap-leg{transform-origin:50% 0%;transition:transform .3s ease}
.ap-tail{transform-origin:95% 95%;transition:transform .3s ease}
.ap-eye-open{transform-origin:50% 50%}
.ap-wing{transform-origin:85% 15%;transition:transform .3s ease}
.ap-arm{transform-origin:8% 8%;transition:transform .3s ease}
.ap-unflip{transform-origin:50% 50%}
.ap-pet[data-facing="left"] .ap-unflip{transform:scaleX(-1)}
.ap-z,.ap-heart,.ap-sparkle,.ap-dirt,.ap-tear,.ap-think-cloud,.ap-bang,.ap-bark-lines,.ap-sound{transform-origin:50% 50%}
.ap-bang{transform-origin:50% 100%}.ap-bark-lines{transform-origin:0% 50%}.ap-sound{transform-origin:50% 100%}
${SP("cat")} .ap-ear,${SP("bird")} .ap-ear,${SP("robot")} .ap-ear{transform-origin:50% 100%}
${SP("cat")} .ap-tail{transform-origin:90% 95%}
${SP("bird")} .ap-tail{transform-origin:100% 40%}

/* ---------- visibility toggles ---------- */
.ap-eye-closed,.ap-eye-happy,.ap-mouth-open,.ap-tongue,.ap-brow,.ap-cheek,
.ap-fx-bed,.ap-fx-zzz,.ap-fx-think,.ap-fx-alert,.ap-fx-sound,.ap-fx-bark,.ap-fx-happy,.ap-fx-sad,.ap-fx-tear,.ap-fx-dig,.ap-fx-plan{display:none}

/* ---------- keyframes ---------- */
@keyframes ap-breathe{0%,100%{transform:scaleY(1)}50%{transform:scaleY(1.035)}}
@keyframes ap-blink{0%,92%,100%{transform:scaleY(1)}95%{transform:scaleY(.1)}}
@keyframes ap-wag-slow{0%,100%{transform:rotate(-6deg)}50%{transform:rotate(10deg)}}
@keyframes ap-wag-fast{0%,100%{transform:rotate(-18deg)}50%{transform:rotate(26deg)}}
@keyframes ap-cat-tail{0%,100%{transform:rotate(-4deg)}50%{transform:rotate(8deg)}}
@keyframes ap-tilt{0%,100%{transform:rotate(-12deg)}50%{transform:rotate(-7deg)}}
@keyframes ap-float-z{0%{opacity:0;transform:translate(0,4px) scale(.6)}30%{opacity:1}100%{opacity:0;transform:translate(6px,-10px) scale(1.1)}}
@keyframes ap-pulse{0%,100%{transform:scale(1)}50%{transform:scale(1.08)}}
@keyframes ap-dig{0%,100%{transform:rotate(-32deg)}50%{transform:rotate(24deg)}}
@keyframes ap-dirt1{0%{opacity:0;transform:translate(0,0)}15%{opacity:1}100%{opacity:0;transform:translate(-30px,-24px)}}
@keyframes ap-dirt2{0%{opacity:0;transform:translate(0,0)}15%{opacity:1}100%{opacity:0;transform:translate(-22px,-30px)}}
@keyframes ap-dirt3{0%{opacity:0;transform:translate(0,0)}15%{opacity:1}100%{opacity:0;transform:translate(-36px,-14px)}}
@keyframes ap-bounce{0%,100%{transform:translateY(0) scaleY(1)}12%{transform:translateY(0) scaleY(.94)}45%{transform:translateY(-11%) scaleY(1.03)}70%{transform:translateY(0) scaleY(1)}}
@keyframes ap-pop{0%{transform:scale(.4);opacity:0}18%{transform:scale(1.18);opacity:1}32%{transform:scale(1)}80%{opacity:1}100%{opacity:0;transform:scale(1)}}
@keyframes ap-bark-head{0%,100%{transform:rotate(0)}20%{transform:rotate(-9deg)}45%{transform:rotate(2deg)}}
@keyframes ap-zoom-move{0%{transform:translateX(0)}25%{transform:translateX(22%)}75%{transform:translateX(-22%)}100%{transform:translateX(0)}}
@keyframes ap-zoom-flip{0%,24.9%{transform:scaleX(1)}25%,74.9%{transform:scaleX(-1)}75%,100%{transform:scaleX(1)}}
@keyframes ap-run{0%,100%{transform:rotate(-30deg)}50%{transform:rotate(30deg)}}
@keyframes ap-walk{0%,100%{transform:rotate(-18deg)}50%{transform:rotate(18deg)}}
@keyframes ap-bob{0%,100%{transform:translateY(0)}50%{transform:translateY(-2.5%)}}
@keyframes ap-heart{0%{opacity:0;transform:translateY(4px) scale(.5)}25%{opacity:1;transform:translateY(0) scale(1)}100%{opacity:0;transform:translateY(-14px) scale(.9)}}
@keyframes ap-twinkle{0%,100%{opacity:.2;transform:scale(.6) rotate(0)}50%{opacity:1;transform:scale(1.1) rotate(45deg)}}
@keyframes ap-tear{0%{opacity:0;transform:translateY(0) scaleY(.6)}20%{opacity:1}100%{opacity:0;transform:translateY(16px) scaleY(1.1)}}
@keyframes ap-rain{0%{opacity:0;transform:translateY(0)}30%{opacity:1}100%{opacity:0;transform:translateY(8px)}}
@keyframes ap-whimper{0%,88%,100%{transform:translate(0,3px)}91%{transform:translate(-1px,3px)}94%{transform:translate(1px,3px)}97%{transform:translate(-1px,3px)}}
@keyframes ap-check{0%,8%{stroke-dashoffset:12}22%,88%{stroke-dashoffset:0}100%{stroke-dashoffset:12}}
@keyframes ap-nod{0%,100%{transform:rotate(2deg)}50%{transform:rotate(-3deg)}}
@keyframes ap-peck{0%,100%{transform:rotate(0)}35%,50%{transform:rotate(34deg)}}
@keyframes ap-flap{0%,100%{transform:rotate(0)}50%{transform:rotate(-38deg)}}
@keyframes ap-light{0%,100%{opacity:1}50%{opacity:.25}}
@keyframes ap-hammer{0%,100%{transform:rotate(-38deg)}50%{transform:rotate(16deg)}}
@keyframes ap-wave{0%,100%{transform:rotate(-70deg)}50%{transform:rotate(-30deg)}}

/* ---------- base / idle ---------- */
.ap-body{animation:ap-breathe 2.6s ease-in-out infinite}
.ap-eye-open{animation:ap-blink 4.6s infinite}
.ap-tail{animation:ap-wag-slow 1.8s ease-in-out infinite}
${SP("cat")} .ap-tail{animation:ap-cat-tail 3.2s ease-in-out infinite}
${SP("bird")} .ap-tail{animation:none}
.ap-light{animation:ap-light 2.2s ease-in-out infinite}
.ap-chest{animation:ap-light 1.6s ease-in-out infinite}.ap-chest2{animation-delay:.5s}.ap-chest3{animation-delay:1s}
.ap-pad-glow{animation:ap-light 2.4s ease-in-out infinite}

/* ---------- napping: lie down in bed, eyes shut, zzz ---------- */
${A("napping")} .ap-fx-bed,${A("napping")} .ap-fx-zzz,${A("napping")} .ap-eye-closed{display:inline}
${A("napping")} .ap-eye-open{display:none}
${A("napping")} .ap-char{transform:translateY(14px)}
${A("napping")} .ap-leg{transform:scaleY(.35);animation:none}
${A("napping")} .ap-head{transform:translate(0,6px) rotate(12deg)}
${A("napping")} .ap-tail{transform:rotate(-55deg);animation:none}
${A("napping")} .ap-body{animation-duration:4.2s}
${A("napping")} .ap-z{animation:ap-float-z 3s ease-in-out infinite}
${A("napping")} .ap-z2{animation-delay:1s}${A("napping")} .ap-z3{animation-delay:2s}
${SP("cat", "napping")} .ap-ear{transform:rotate(-14deg)}
${SP("bird", "napping")} .ap-head{transform:translate(-5px,7px) rotate(-18deg)}
${SP("bird", "napping")} .ap-tail{transform:rotate(10deg)}
${SP("robot", "napping")} .ap-char{transform:translateY(6px)}
${SP("robot", "napping")} .ap-head{transform:translate(0,3px) rotate(6deg)}
${SP("robot", "napping")} .ap-light,${SP("robot", "napping")} .ap-chest{animation:none;opacity:.25}

/* ---------- thinking: head tilt, ears up, thought bubble ---------- */
${A("thinking")} .ap-fx-think{display:inline}
${A("thinking")} .ap-head{transform:rotate(-12deg);animation:ap-tilt 2.4s ease-in-out infinite}
${A("thinking")} .ap-think-cloud{animation:ap-pulse 1.6s ease-in-out infinite}
${A("thinking")} .ap-tail{animation-duration:2.6s}
${SP("dog", "thinking")} .ap-ear{transform:rotate(38deg)}
${SP("cat", "thinking")} .ap-ear{transform:scaleY(1.15)}
${SP("bird", "thinking")} .ap-ear{transform:scaleY(1.3)}
${SP("robot", "thinking")} .ap-ear{transform:translateY(-2px)}
${SP("robot", "thinking")} .ap-light{fill:#ffd84a;animation-duration:.8s}

/* ---------- working: dig and fetch ---------- */
${A("working")} .ap-fx-dig{display:inline}
${A("working")} .ap-char{transform:rotate(7deg)}
${A("working")} .ap-head{transform:rotate(10deg)}
${A("working")} .ap-leg-f1,${A("working")} .ap-leg-f2{animation:ap-dig .32s ease-in-out infinite}
${A("working")} .ap-leg-f2{animation-delay:-.16s}
${A("working")} .ap-tail{animation:ap-wag-fast .42s ease-in-out infinite}
${A("working")} .ap-dirt1{animation:ap-dirt1 .64s ease-out infinite}
${A("working")} .ap-dirt2{animation:ap-dirt2 .64s ease-out infinite;animation-delay:.16s}
${A("working")} .ap-dirt3{animation:ap-dirt3 .64s ease-out infinite;animation-delay:.32s}
${A("working")} .ap-dirt4{animation:ap-dirt1 .64s ease-out infinite;animation-delay:.48s}
${SP("dog", "working")} .ap-ear{transform:rotate(18deg)}
${SP("bird", "working")} .ap-char{transform:none}
${SP("bird", "working")} .ap-head{animation:ap-peck .45s ease-in-out infinite}
${SP("bird", "working")} .ap-leg{animation:none}
${SP("robot", "working")} .ap-char{transform:none}
${SP("robot", "working")} .ap-head{transform:rotate(6deg)}
${SP("robot", "working")} .ap-leg{animation:none}
${SP("robot", "working")} .ap-arm{animation:ap-hammer .35s ease-in-out infinite}

/* ---------- planning: carry the checklist ---------- */
${A("planning")} .ap-fx-plan{display:inline}
${A("planning")} .ap-head{animation:ap-nod 1.8s ease-in-out infinite}
${A("planning")} .ap-tail{animation-duration:1.1s}
${A("planning")} .ap-check{animation:ap-check 3.6s ease-in-out infinite}
${A("planning")} .ap-check2{animation-delay:.9s}${A("planning")} .ap-check3{animation-delay:1.8s}
${SP("dog", "planning")} .ap-ear{transform:rotate(14deg)}
${SP("robot", "planning")} .ap-head{animation:none;transform:rotate(4deg)}
${SP("robot", "planning")} .ap-arm{transform:rotate(-24deg)}

/* ---------- alert: bark, bounce, ears up ---------- */
${A("alert")} .ap-fx-alert,${A("alert")} .ap-fx-sound,${A("alert")} .ap-fx-bark,${A("alert")} .ap-mouth-open{display:inline}
${A("alert")} .ap-mouth-closed{display:none}
${A("alert")} .ap-move{animation:ap-bounce .55s ease-in-out infinite}
${A("alert")} .ap-head{animation:ap-bark-head .55s ease-in-out infinite}
${A("alert")} .ap-tail{transform:rotate(14deg);animation:ap-wag-fast .3s ease-in-out infinite}
${A("alert")} .ap-bang{animation:ap-pop 1.1s ease-out infinite}
${A("alert")} .ap-sound{animation:ap-pop 1.1s ease-out infinite;animation-delay:.1s}
${A("alert")} .ap-bark-lines{animation:ap-pop .55s ease-out infinite}
${SP("dog", "alert")} .ap-ear{transform:rotate(52deg)}
${SP("cat", "alert")} .ap-ear{transform:scaleY(1.2) rotate(-4deg)}
${SP("bird", "alert")} .ap-ear{transform:scaleY(1.35)}
${SP("bird", "alert")} .ap-wing{animation:ap-flap .26s ease-in-out infinite}
${SP("robot", "alert")} .ap-light{fill:#ff3b3b;animation-duration:.35s}
${SP("robot", "alert")} .ap-arm{animation:ap-wave .5s ease-in-out infinite}

/* ---------- happy: tail wag + zoomies ---------- */
${A("happy")} .ap-fx-happy,${A("happy")} .ap-mouth-open,${A("happy")} .ap-tongue,${A("happy")} .ap-eye-happy,${A("happy")} .ap-cheek{display:inline}
${A("happy")} .ap-mouth-closed,${A("happy")} .ap-eye-open{display:none}
${A("happy")} .ap-zoom{animation:ap-zoom-move 2.2s ease-in-out infinite}
${A("happy")} .ap-zoom-flip{animation:ap-zoom-flip 2.2s linear infinite}
.ap-pet[data-facing="left"][data-activity="happy"] .ap-zoom,.ap-pet[data-facing="left"][data-activity="happy"] .ap-zoom-flip{animation-direction:reverse}
${A("happy")} .ap-move{animation:ap-bob .28s ease-in-out infinite}
${A("happy")} .ap-leg-f1,${A("happy")} .ap-leg-b2{animation:ap-run .28s linear infinite}
${A("happy")} .ap-leg-f2,${A("happy")} .ap-leg-b1{animation:ap-run .28s linear infinite reverse}
${A("happy")} .ap-tail{animation:ap-wag-fast .18s linear infinite}
${A("happy")} .ap-heart1{animation:ap-heart 1.4s ease-out infinite}
${A("happy")} .ap-heart2{animation:ap-heart 1.4s ease-out infinite;animation-delay:.7s}
${A("happy")} .ap-sparkle1{animation:ap-twinkle 1s ease-in-out infinite}
${A("happy")} .ap-sparkle2{animation:ap-twinkle 1s ease-in-out infinite;animation-delay:.5s}
${SP("dog", "happy")} .ap-ear{transform:rotate(30deg)}
${SP("bird", "happy")} .ap-wing{animation:ap-flap .3s ease-in-out infinite}
${SP("robot", "happy")} .ap-mouth-open{display:none}
${SP("robot", "happy")} .ap-arm{animation:ap-wave .6s ease-in-out infinite}

/* ---------- sad: droop and whine ---------- */
${A("sad")} .ap-fx-sad,${A("sad")} .ap-fx-tear,${A("sad")} .ap-brow{display:inline}
${A("sad")} .ap-char{transform:translateY(3px);animation:ap-whimper 3s infinite}
${A("sad")} .ap-head{transform:translateY(3px) rotate(16deg)}
${A("sad")} .ap-tail{transform:rotate(-70deg);animation:none}
${A("sad")} .ap-body{animation-duration:3.6s}
${A("sad")} .ap-tear{animation:ap-tear 2.2s ease-in infinite}
${A("sad")} .ap-rain{animation:ap-rain 1s linear infinite}
${A("sad")} .ap-rain2{animation-delay:.33s}${A("sad")} .ap-rain3{animation-delay:.66s}
${SP("dog", "sad")} .ap-ear{transform:translateY(2px) rotate(-4deg)}
${SP("cat", "sad")} .ap-ear{transform:rotate(-38deg)}
${SP("cat", "sad")} .ap-tail{transform:rotate(-40deg)}
${SP("bird", "sad")} .ap-ear{transform:rotate(-35deg)}
${SP("bird", "sad")} .ap-tail{transform:rotate(-18deg)}
${SP("bird", "sad")} .ap-wing{transform:rotate(8deg) translateY(2px)}
${SP("robot", "sad")} .ap-head{transform:translateY(2px) rotate(9deg)}
${SP("robot", "sad")} .ap-ear{transform:rotate(28deg)}
${SP("robot", "sad")} .ap-light{opacity:.35;animation:none}

/* ---------- walk cycle (after activities so it wins) ---------- */
${W} .ap-leg-f1,${W} .ap-leg-b2{animation:ap-walk .5s ease-in-out infinite}
${W} .ap-leg-f2,${W} .ap-leg-b1{animation:ap-walk .5s ease-in-out infinite reverse}
${W} .ap-move{animation:ap-bob .25s ease-in-out infinite}
${W} .ap-tail{animation:ap-wag-slow .7s ease-in-out infinite}
${SP("bird")}[data-walking="true"] .ap-leg-b1{animation:ap-walk .36s ease-in-out infinite reverse}
${SP("bird")}[data-walking="true"] .ap-leg-f1{animation:ap-walk .36s ease-in-out infinite}

/* ---------- bubble ---------- */
.ap-bubble{position:relative;box-sizing:border-box;max-width:240px;padding:8px 11px;border-radius:14px;background:#fff;color:#2b2b33;
  font:600 12.5px/1.35 ui-rounded,system-ui,-apple-system,"Segoe UI",sans-serif;box-shadow:0 4px 14px rgba(0,0,0,.18),0 0 0 1px rgba(0,0,0,.06);
  pointer-events:auto;cursor:default;animation:ap-bubble-in .22s ease-out;word-wrap:break-word;overflow-wrap:anywhere}
.ap-bubble::after{content:"";position:absolute;left:clamp(14px,calc(50% + var(--ap-tail-x,0px)),calc(100% - 14px));bottom:-6px;width:12px;height:12px;margin-left:-6px;background:inherit;
  transform:rotate(45deg);box-shadow:3px 3px 4px rgba(0,0,0,.06);border-bottom-right-radius:3px}
.ap-bubble[data-clickable="true"]{cursor:pointer}
.ap-bubble-permission{background:#fff7e6;box-shadow:0 4px 16px rgba(255,140,0,.28),0 0 0 1.5px #ffb347}
.ap-bubble-text{position:relative;z-index:1}
.ap-bubble-actions{position:relative;z-index:1;display:flex;gap:6px;margin-top:7px}
.ap-btn{flex:1;min-width:0;appearance:none;border:0;border-radius:9px;padding:5px 10px;font:700 12px/1.2 ui-rounded,system-ui,-apple-system,"Segoe UI",sans-serif;cursor:pointer;
  transition:transform .08s ease,filter .15s ease}
.ap-btn:hover{filter:brightness(1.06)}.ap-btn:active{transform:scale(.96)}
.ap-btn:focus-visible{outline:2px solid #3f8cff;outline-offset:2px}
.ap-btn[disabled]{opacity:.55;cursor:default;transform:none}
.ap-btn-approve{background:#2fa84f;color:#fff}
.ap-btn-deny{background:#eceef2;color:#3a3f48}
@keyframes ap-bubble-in{from{opacity:0;transform:translateY(4px) scale(.96)}to{opacity:1;transform:none}}

/* ---------- stage ---------- */
.ap-stage{position:relative;overflow:visible;pointer-events:none}
.ap-stage-pet{position:absolute;left:0;bottom:0;pointer-events:auto;will-change:transform;outline:none}
.ap-stage-pet[role="button"]{cursor:pointer}
.ap-stage-pet:focus-visible{outline:2px solid #3f8cff;outline-offset:2px;border-radius:12px}
.ap-stage-name{position:absolute;left:50%;bottom:-16px;transform:translateX(-50%);white-space:nowrap;padding:1px 7px;border-radius:8px;
  background:rgba(20,22,28,.62);color:#fff;font:700 10.5px/1.4 ui-rounded,system-ui,-apple-system,"Segoe UI",sans-serif;pointer-events:none}
.ap-stage-bubble{position:absolute;transform:translateX(-50%);z-index:2;pointer-events:none;width:max-content;max-width:var(--ap-bubble-max,240px)}
.ap-stage-bubble .ap-bubble{max-width:100%}

/* ---------- reduced motion: keep static poses, drop movement ---------- */
@media (prefers-reduced-motion: reduce){
  .ap-pet *,.ap-pet,.ap-bubble{animation:none!important;transition:none!important}
  .ap-stage-pet{transition:none!important}
  .ap-check{stroke-dashoffset:0!important}
}
.ap-reduced-motion .ap-pet *,.ap-reduced-motion.ap-pet *,.ap-reduced-motion .ap-bubble{animation:none!important;transition:none!important}
`;

const STYLE_ID = "ai-pet-engine-styles";

interface StyleConfig {
  inject: boolean;
  nonce?: string;
}

const config: StyleConfig = { inject: true };

/**
 * Configure automatic style injection. Pass `{ nonce }` for CSP `style-src 'nonce-…'`, or
 * `{ inject: false }` if the host ships `PET_CSS` itself (e.g. as a file in a VS Code webview).
 */
export function configurePetStyles(opts: { inject?: boolean; nonce?: string }): void {
  if (opts.inject !== undefined) config.inject = opts.inject;
  if (opts.nonce !== undefined) config.nonce = opts.nonce;
}

/** Inject PET_CSS once per document (idempotent; no-op without a DOM or when disabled). */
export function ensurePetStyles(doc: Document | undefined = typeof document !== "undefined" ? document : undefined): void {
  if (!config.inject || !doc || !doc.head) return;
  if (doc.getElementById(STYLE_ID)) return;
  const el = doc.createElement("style");
  el.id = STYLE_ID;
  if (config.nonce) el.setAttribute("nonce", config.nonce);
  el.textContent = PET_CSS;
  doc.head.appendChild(el);
}
