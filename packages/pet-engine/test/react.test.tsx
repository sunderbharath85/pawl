import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { Species } from "@ai-pet/protocol";
import { PET_ACTIVITIES, parseSpriteManifest, type PetView } from "../src/index";
import { bubblePlacement, computeLanes, PET_CSS, PetBubble, PetSprite, PetStage } from "../src/react/index";
import { currentX, haltMotion, planWander, restingMotion } from "../src/react/stage-logic";

const SPECIES: Species[] = ["dog", "cat", "bird", "robot"];

function view(over: Partial<PetView> = {}): PetView {
  return { petId: "p1", agentId: "claude-code", species: "dog", name: "Biscuit", activity: "idle", since: 0, mood: 70, ...over };
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
