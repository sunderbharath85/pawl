import gsap from "gsap";
import { ScrollTrigger } from "gsap/ScrollTrigger";
import Lenis from "lenis";
import { createScene } from "./scene.js";

gsap.registerPlugin(ScrollTrigger);

const REPO = "sunderbharath85/pawl";
const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const $ = (s, root = document) => root.querySelector(s);
const $$ = (s, root = document) => Array.from(root.querySelectorAll(s));

// Wrap each word of an element's text in <span class="w">.
function splitWords(el) {
  const words = el.textContent.trim().split(/\s+/);
  el.textContent = "";
  return words.map((word, i) => {
    const span = document.createElement("span");
    span.className = "w";
    span.textContent = word;
    el.appendChild(span);
    if (i < words.length - 1) el.appendChild(document.createTextNode(" "));
    return span;
  });
}

// ---------------------------------------------------------------- smooth scroll
let lenis = null;
if (!reduced) {
  lenis = new Lenis({ autoRaf: false, lerp: 0.09 });
  lenis.on("scroll", ScrollTrigger.update);
  gsap.ticker.add((time) => lenis.raf(time * 1000));
  gsap.ticker.lagSmoothing(0);
  $$('a[href^="#"]').forEach((a) => {
    a.addEventListener("click", (e) => {
      const target = $(a.getAttribute("href"));
      if (!target) return;
      e.preventDefault();
      lenis.scrollTo(target, { offset: a.getAttribute("href") === "#top" ? 0 : -40, duration: 1.4 });
    });
  });
}

// ---------------------------------------------------------------- three.js stage
let scene = null;
try {
  scene = createScene($(".stage__canvas"), { reduced });
} catch (err) {
  console.warn("WebGL unavailable, showing the static backdrop", err);
  $(".stage").classList.add("is-fallback");
}
if (scene) {
  if (reduced) scene.requestRender();
  else scene.start();
  if (lenis) gsap.ticker.add(() => scene.setScrollVelocity(lenis.velocity * 60));
  ScrollTrigger.create({
    trigger: ".light",
    start: "top top",
    onEnter: () => scene.stop(),
    onLeaveBack: () => (reduced ? scene.requestRender() : scene.start()),
  });
  if (!reduced) {
    gsap.to(scene.state, {
      t: 1,
      ease: "none",
      scrollTrigger: { trigger: ".why", start: "top bottom", end: "center center", scrub: true },
    });
  }
}

// ---------------------------------------------------------------- hero
if (!reduced) {
  const intro = gsap.timeline({ defaults: { ease: "expo.out" } });
  intro
    .from(".stage__canvas", { opacity: 0, duration: 1.8, ease: "power2.out" }, 0)
    .from(".line__in", { yPercent: 115, duration: 1.3, stagger: 0.12 }, 0.15)
    .from([".hero__lede", ".hero__ctas", ".hero__fine"], { y: 26, opacity: 0, duration: 1.1, stagger: 0.09 }, 0.55)
    .from(".nav", { y: -30, opacity: 0, duration: 1 }, 0.4);
  gsap.to(".hero__inner", {
    opacity: 0,
    y: -80,
    ease: "none",
    scrollTrigger: { trigger: ".hero", start: "top top", end: "bottom 25%", scrub: true },
  });
}

// ---------------------------------------------------------------- scrubbing text reveal
$$("[data-scrub]").forEach((el) => {
  const words = splitWords(el);
  if (reduced) return;
  gsap.to(words, {
    opacity: 1,
    ease: "none",
    stagger: 0.08,
    scrollTrigger: { trigger: el, start: "top 78%", end: "bottom 42%", scrub: true },
  });
});

// ---------------------------------------------------------------- nav theme
const nav = $(".nav");
ScrollTrigger.create({
  trigger: ".light",
  start: "top 64px",
  end: "bottom 64px",
  onToggle: (self) => (nav.dataset.theme = self.isActive ? "light" : "dark"),
});

// ---------------------------------------------------------------- reveals
if (!reduced) {
  gsap.from(".inline-head", {
    y: 70, opacity: 0, duration: 1.3, ease: "expo.out",
    scrollTrigger: { trigger: ".inline-head", start: "top 85%" },
  });
  gsap.from(".pill", {
    width: 0, duration: 1.4, ease: "expo.out", stagger: 0.15, delay: 0.25,
    scrollTrigger: { trigger: ".inline-head", start: "top 85%" },
  });
  gsap.from(".card", {
    y: 90, opacity: 0, duration: 1.2, ease: "expo.out", stagger: 0.08,
    scrollTrigger: { trigger: ".bento", start: "top 82%" },
  });
  $$(".day__head, .everywhere__title, .overheard").forEach((el) => {
    gsap.from(el, { y: 60, opacity: 0, duration: 1.2, ease: "expo.out", scrollTrigger: { trigger: el, start: "top 85%" } });
  });
  gsap.from(".cta__title", {
    yPercent: 35, opacity: 0, duration: 1.4, ease: "expo.out",
    scrollTrigger: { trigger: ".cta", start: "top 75%" },
  });
}

// ---------------------------------------------------------------- card stacking
const slabs = $$(".slab");
if (!reduced) {
  slabs.forEach((slab, i) => {
    const next = slabs[i + 1];
    if (!next) return;
    // fromTo with an explicit brightness(1): tweening from the computed "none" starts at brightness(0),
    // which left every card black until the next one covered it.
    gsap.fromTo(
      slab,
      { scale: 1, y: 0, filter: "brightness(1)" },
      {
        scale: 0.9,
        y: -24,
        filter: "brightness(0.82)",
        ease: "none",
        scrollTrigger: { trigger: next, start: "top bottom", end: "top 14%", scrub: true },
      },
    );
  });
}

// ---------------------------------------------------------------- pet showcase (bento card)
const STATES = [
  ["idle", 6],
  ["working", 8],
  ["alert", 10],
  ["happy", 10],
  ["napping", 3],
];
const showcase = $("[data-showcase]");
const stateLabel = $("[data-state]");
let species = "dog";
let stateIndex = 0;
function paintShowcase() {
  const [name, fps] = STATES[stateIndex];
  showcase.style.setProperty("--src", `url(pets/${species}/${name}.png)`);
  showcase.style.setProperty("--fps", fps);
  stateLabel.textContent = name === "alert" ? "alert: needs you" : name;
}
$$(".chip[data-species]").forEach((chip) => {
  chip.addEventListener("click", () => {
    $$(".chip[data-species]").forEach((c) => c.classList.toggle("is-on", c === chip));
    species = chip.dataset.species;
    paintShowcase();
    if (!reduced) gsap.fromTo(showcase, { scale: 0.86, y: 14 }, { scale: 1, y: 0, duration: 0.8, ease: "elastic.out(1, 0.5)" });
  });
});
// preload every frame strip the showcase can show
["dog", "cat", "bird", "robot"].forEach((s) => STATES.forEach(([n]) => { new Image().src = `pets/${s}/${n}.png`; }));
let showcaseVisible = false;
ScrollTrigger.create({ trigger: ".card--a", onToggle: (self) => (showcaseVisible = self.isActive) });
setInterval(() => {
  if (!showcaseVisible || reduced) return;
  stateIndex = (stateIndex + 1) % STATES.length;
  paintShowcase();
}, 1900);

// ---------------------------------------------------------------- horizontal accordion
const folds = $$(".fold");
function openFold(fold) {
  folds.forEach((f) => {
    f.classList.toggle("is-open", f === fold);
    f.setAttribute("aria-expanded", f === fold ? "true" : "false");
  });
}
folds.forEach((fold) => {
  fold.setAttribute("role", "button");
  fold.setAttribute("aria-expanded", fold.classList.contains("is-open") ? "true" : "false");
  fold.addEventListener("mouseenter", () => openFold(fold));
  fold.addEventListener("focus", () => openFold(fold));
  fold.addEventListener("click", () => openFold(fold));
  fold.addEventListener("keydown", (e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      openFold(fold);
    }
  });
});

// ---------------------------------------------------------------- overheard carousel
const QUOTES = [
  ["Woof! Codex wants to run bun test. Can I let it?", "Juniper, watching Codex on Desk PC"],
  ["Claude finished the refactor. Fourteen files, and the tests are green.", "Miso, watching Claude Code on Laptop"],
  ["The agent on Laptop has been quiet for twenty minutes. Should I nudge it?", "Kiwi, the watchdog on Laptop"],
  ["A rule matched: no rm -rf outside the repo. I said no for you.", "Bolt, watching Gemini CLI on Studio PC"],
];
const faces = $$(".face");
const quoteText = $(".quote__text");
const quoteWho = $(".quote__who");
let quoteIndex = 0;
function showQuote(i, animate = true) {
  quoteIndex = (i + QUOTES.length) % QUOTES.length;
  const [text, who] = QUOTES[quoteIndex];
  faces.forEach((f, k) => f.classList.toggle("is-on", k === quoteIndex));
  quoteText.textContent = text;
  quoteWho.textContent = who;
  if (!animate || reduced) return;
  const words = splitWords(quoteText);
  gsap.fromTo(words, { y: 24, opacity: 0 }, { y: 0, opacity: 1, duration: 0.7, stagger: 0.03, ease: "expo.out" });
  gsap.fromTo(quoteWho, { opacity: 0 }, { opacity: 1, duration: 0.6, delay: 0.3 });
}
showQuote(0, false);
$$(".round[data-dir]").forEach((b) => b.addEventListener("click", () => {
  showQuote(quoteIndex + Number(b.dataset.dir));
  lastManual = Date.now();
}));
let carouselVisible = false;
let lastManual = 0;
const carousel = $(".overheard");
let hovering = false;
carousel.addEventListener("mouseenter", () => (hovering = true));
carousel.addEventListener("mouseleave", () => (hovering = false));
ScrollTrigger.create({ trigger: carousel, start: "top 70%", end: "bottom 30%", onToggle: (self) => (carouselVisible = self.isActive) });
setInterval(() => {
  if (reduced || !carouselVisible || hovering || Date.now() - lastManual < 9000) return;
  showQuote(quoteIndex + 1);
}, 5200);

// ---------------------------------------------------------------- downloads: point at the newest release
fetch(`https://api.github.com/repos/${REPO}/releases?per_page=5`, { headers: { Accept: "application/vnd.github+json" } })
  .then((r) => (r.ok ? r.json() : []))
  .then((releases) => {
    const rel = releases.find((r) => !r.draft);
    if (!rel) return;
    const pick = (test) => rel.assets.find((a) => test(a.name))?.browser_download_url;
    const links = {
      exe: pick((n) => n.endsWith("-setup.exe")),
      msi: pick((n) => n.endsWith(".msi")),
      zip: pick((n) => n.startsWith("pawl-cli-") && n.endsWith(".zip")),
    };
    $$("[data-dl]").forEach((a) => {
      const url = links[a.dataset.dl];
      if (url) a.href = url;
    });
    $$("[data-dl-ver]").forEach((el) => (el.textContent = rel.tag_name));
  })
  .catch(() => {});

document.fonts?.ready.then(() => ScrollTrigger.refresh());
