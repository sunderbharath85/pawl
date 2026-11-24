import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { Species } from "@krypto/protocol";
import { PET_ACTIVITIES, parseSpriteManifest, type PetView } from "../src/index";
import { bubblePlacement, computeLanes, PET_CSS, PetBubble, PetSprite, PetStage } from "../src/react/index";
import { currentX, haltMotion, layoutVisitors, offscreenX, planWander, restingMotion, walkMotion } from "../src/react/stage-logic";

const SPECIES: Species[] = ["dog", "cat", "bird", "robot"];

function view(over: Partial<PetView> = {}): PetView {
  return { petId: "winpc0000001:p1", machineId: "winpc0000001", agentId: "claude-code", species: "dog", name: "Biscuit", activity: "idle", since: 0, mood: 70, visitor: false, remoteOffline: false, ...over } as PetView;
}

describe("PetSprite (SSR)", () => {
  test("renders every species × activity as SVG without throwing", () => {
    for (const species of SPECIES) {
      for (const activity of PET_ACTIVITIES) {
        const html = renderToStaticMarkup(<PetSprite species={species} activity={activity} size={64} facing="left" walking />);
        expect(html).toContain(`ap-sp-${species}`);
        expect(html).toContain(`data-activity="${activity}"`);
        expect(html).toContain('data-facing="left"');
        expect(html).toContain("<svg");
      }
    }
  });

  test("falls back to SVG until the sprite image is known to load", () => {
    const manifest = parseSpriteManifest({
      version: 1,
      frameSize: 256,
      species: { dog: { skins: ["default"], animations: { idle: { src: "dog/idle.png", frames: 4, fps: 6, loop: true } } } },
    })!;
    const html = renderToStaticMarkup(<PetSprite species="dog" activity="idle" sprites={{ manifest, baseUrl: "/pets" }} />);
    expect(html).toContain("<svg");
    expect(html).not.toContain("ap-has-strip");
  });
});

describe("PetBubble (SSR)", () => {
  test("null without bubble", () => {
    expect(renderToStaticMarkup(<PetBubble view={view()} />)).toBe("");
  });
  test("permission bubble has Approve/Deny and hit marker", () => {
    const html = renderToStaticMarkup(
      <PetBubble view={view({ bubble: { kind: "permission", text: "Woof! Run tests?", requestId: "r1" } })} onApprove={() => {}} onDeny={() => {}} />,
    );
    expect(html).toContain("Approve");
    expect(html).toContain("Deny");
    expect(html).toContain('data-pet-hit="bubble"');
    expect(html).toContain("Woof! Run tests?");
  });
  test("say bubble has no buttons", () => {
    const html = renderToStaticMarkup(<PetBubble view={view({ bubble: { kind: "say", text: "Done!" } })} onApprove={() => {}} />);
    expect(html).not.toContain("Approve");
  });
});

describe("PetStage (SSR)", () => {
  test("lays out pets in lanes with hit markers and bubbles", () => {
    const views = [
      view({ petId: "a", name: "A" }),
      view({ petId: "b", name: "B", species: "robot", activity: "alert", bubble: { kind: "permission", text: "Beep?", requestId: "r9" } }),
    ];
    const html = renderToStaticMarkup(<PetStage views={views} width={400} onApprove={() => {}} onDeny={() => {}} onPetClick={() => {}} showNames />);
    expect(html.match(/data-pet-hit="pet"/g)?.length).toBe(2);
    expect(html).toContain('data-pet-hit="bubble"');
    expect(html).toContain("translateX(52.0px)"); // lane 0 center 100 - 48
    expect(html).toContain("translateX(252.0px)"); // lane 1 center 300 - 48
    expect(html).toContain('role="button"');
  });
});

describe("PetStage names", () => {
  const views = [view({ petId: "a", name: "Biscuit" }), view({ petId: "b", name: "Pepper", machineName: "Mac mini" })];
  const render = (extra: { showNames?: boolean } = {}) =>
    renderToStaticMarkup(<PetStage views={views} width={400} onApprove={() => {}} onDeny={() => {}} {...extra} />);

  test("shows each pet's name by default", () => {
    const html = render();
    expect(html.match(/class="ap-stage-name"/g)?.length).toBe(2);
    expect(html).toContain(">Biscuit</div>");
    expect(html).toContain(">Pepper</div>");
    expect(html).not.toContain("Mac mini"); // name only; visitor labels come in step 3
  });

  test("showNames={false} hides the name tags and the room for them", () => {
    const shown = render();
    const hidden = render({ showNames: false });
    expect(hidden).not.toContain("ap-stage-name");
    expect(shown).toContain("height:202px"); // 96 pet + 88 bubble room + 18 name room
    expect(hidden).toContain("height:184px");
  });
});

describe("PetStage visitors (SSR)", () => {
  const MAC = "macmini00002";
  const visitor = (n: number, over: Partial<PetView> = {}) =>
    view({ petId: `${MAC}:p${n}`, machineId: MAC, machineName: "Mac mini", name: `Guest${n}`, visitor: true, ...over });
  const render = (views: PetView[], width = 1200) =>
    renderToStaticMarkup(<PetStage views={views} width={width} petSize={96} onApprove={() => {}} onDeny={() => {}} onPetClick={() => {}} />);

  test("visitors sit at the right edge, smaller, with a machine badge", () => {
    const html = render([view({ petId: "a", name: "Biscuit" }), visitor(1)]);
    expect(html).toContain(">Guest1 · Mac mini</div>");
    expect(html).toContain(">Biscuit</div>");
    expect(html).toContain("ap-stage-visitor");
    expect(html).toContain("width:72px"); // 96 × 0.75
    // Home lane shrinks to make room: one lane over 1200 - (82 + 8) px.
    expect(html).toContain(`translateX(${(1110 / 2 - 48).toFixed(1)}px)`);
  });

  test("pets of offline machines are not drawn", () => {
    const html = render([view({ petId: "a" }), visitor(1, { visitor: false, remoteOffline: true })]);
    expect(html).not.toContain("Guest1");
    expect(html.match(/data-pet-hit="pet"/g)?.length).toBe(1);
  });

  test("at most 6 visitors, then +N", () => {
    const html = render([view({ petId: "a" }), ...[1, 2, 3, 4, 5, 6, 7, 8].map((n) => visitor(n))]);
    expect(html.match(/ap-stage-visitor/g)?.length).toBe(6);
    expect(html).toContain(">+2</div>");
    expect(html).toContain("Guest7 · Mac mini");
  });

  test("a visitor that ran here leads, with a read-only bubble (no buttons)", () => {
    const ran = visitor(9, { activity: "alert", bubble: { kind: "permission", text: "Run tests? Needs approval on Mac mini.", requestId: `${MAC}:r1`, readOnly: true } });
    const html = render([visitor(1), ran]);
    expect(html).toContain("Needs approval on Mac mini");
    expect(html).toContain("ap-bubble-readonly");
    expect(html).not.toContain("ap-btn-approve");
    expect(html.indexOf("Guest9")).toBeLessThan(html.indexOf("Guest1"));
  });

  test("an away pet is off-screen, hidden and without its bubble", () => {
    const html = render([
      view({ petId: "a", name: "Biscuit", activity: "alert", away: MAC, bubble: { kind: "permission", text: "Run?", requestId: "r1" } }),
    ]);
    expect(html).toContain("translateX(-144.0px)"); // -96 - 48
    expect(html).toContain("visibility:hidden");
    expect(html).not.toContain("ap-bubble");
    expect(html).not.toContain('data-pet-hit="pet"');
  });
});

describe("stage logic", () => {
  test("computeLanes splits evenly and pins narrow lanes", () => {
    const lanes = computeLanes(["a", "b"], 400, 96);
    expect(lanes.a).toEqual({ min: 48, max: 152, home: 100 });
    expect(lanes.b).toEqual({ min: 248, max: 352, home: 300 });
    const tight = computeLanes(["a", "b", "c"], 200, 96);
    expect(tight.a!.min).toBe(tight.a!.max);
  });
  test("planWander stays in lane and faces its direction", () => {
    const lane = { min: 48, max: 152, home: 100 };
    let r = 0;
    const seq = [0.9, 0.95];
    const m = planWander(restingMotion(100), lane, 1000, 40, () => seq[r++]!, 20)!;
    expect(m.walking).toBe(true);
    expect(m.x).toBeGreaterThanOrEqual(48);
    expect(m.x).toBeLessThanOrEqual(152);
    expect(m.facing).toBe("right");
    expect(m.durationMs).toBe(Math.round((Math.abs(m.x - 100) / 40) * 1000));
    expect(planWander(restingMotion(100), lane, 0, 40, () => 0.1, 20)).toBeNull();
    expect(planWander(restingMotion(100), { min: 100, max: 100, home: 100 }, 0, 40, () => 0.9, 20)).toBeNull();
  });
  test("haltMotion interpolates position", () => {
    const m = { x: 200, fromX: 100, startedAt: 0, durationMs: 1000, walking: true, facing: "right" as const };
    expect(currentX(m, 500)).toBe(150);
    expect(haltMotion(m, 250)).toMatchObject({ x: 125, walking: false });
  });
  test("bubblePlacement clamps into the strip", () => {
    expect(bubblePlacement(10, 400, 200)).toEqual({ center: 104, tail: -82 });
    expect(bubblePlacement(200, 400, 200).center).toBe(200);
    expect(bubblePlacement(50, 100, 200).center).toBe(50);
  });
});

test("PET_CSS covers reduced motion and every activity", () => {
  expect(PET_CSS).toContain("prefers-reduced-motion: reduce");
  // idle is the base styling; every other activity has its own rules.
  const missing = PET_ACTIVITIES.filter((a) => a !== "idle" && !PET_CSS.includes(`data-activity="${a}"`));
  expect(missing).toEqual([]);
  expect(PET_CSS).toContain('data-walking="true"');
});

describe("visitor layout", () => {
  test("right-aligned slots, first id nearest the edge, +N past the cap", () => {
    const l = layoutVisitors(["a", "b", "c"], 1000, 72, 2);
    expect(Object.keys(l.slots)).toEqual(["a", "b"]);
    expect(l.hidden).toEqual(["c"]);
    expect(l.slots.a!).toBeGreaterThan(l.slots.b!);
    expect(l.moreX).toBe(1000 - 4 - 18);
    expect(l.slots.a!).toBe(1000 - 4 - 36 - 41);
  });

  test("never more than half the strip (at least one visitor)", () => {
    expect(Object.keys(layoutVisitors(["a", "b", "c", "d"], 300, 72).slots)).toHaveLength(1);
    expect(layoutVisitors([], 300, 72)).toEqual({ slots: {}, hidden: [], areaWidth: 0, moreX: null });
  });

  test("walkMotion travels from the current position and offscreenX is past the edges", () => {
    const m = walkMotion(restingMotion(100), 400, 0, 100);
    expect(m).toMatchObject({ x: 400, fromX: 100, durationMs: 3000, walking: true, travel: true, facing: "right" });
    expect(walkMotion(m, 100, 1500, 100)).toMatchObject({ fromX: 250, x: 100, facing: "left" });
    expect(walkMotion(restingMotion(5), 5, 0, 100).walking).toBe(false);
    expect(offscreenX("left", 800, 96)).toBe(-96);
    expect(offscreenX("right", 800, 72)).toBe(872);
  });
});
