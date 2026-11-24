import type { CSSProperties, JSX } from "react";
import type { Species } from "@ai-pet/protocol";
import type { PetActivity } from "../activity";
import { SPECIES_META } from "../lines";
import { resolveSpriteAnimation, spriteUrl, type PetSpritesConfig, type SpriteAnimation } from "../sprites";
import { SpeciesSvg } from "./art";
import { useImageStatus, usePetStyles } from "./hooks";

export interface PetSpriteProps {
  species: Species;
  activity: PetActivity;
  /** Edge of the square box in px. Default 96. */
  size?: number | undefined;
  facing?: "left" | "right" | undefined;
  walking?: boolean | undefined;
  /** Palette for the SVG fallback, e.g. dog "golden" | "black" | "white" | "choco". Unknown → default. */
  skin?: string | undefined;
  /** Sprite sheets from art-sprites. Used when the species/activity strip exists and loads; else SVG. */
  sprites?: PetSpritesConfig | null | undefined;
  className?: string | undefined;
  /** Accessible label. Default "<Species>: <activity>". */
  title?: string | undefined;
  style?: CSSProperties | undefined;
}

function stripStyle(anim: SpriteAnimation, url: string, size: number): CSSProperties {
  const frames = Math.max(1, Math.floor(anim.frames));
  const style: CSSProperties & Record<string, string | number> = {
    backgroundImage: `url(${JSON.stringify(url)})`,
    backgroundSize: `${frames * size}px ${size}px`,
  };
  if (frames > 1) {
    const steps = anim.loop ? frames : frames - 1;
    style["--ap-strip-end"] = `${-steps * size}px`;
    style.animation = `ap-strip ${(frames / anim.fps).toFixed(3)}s steps(${steps}) ${anim.loop ? "infinite" : "1 forwards"}`;
  }
  return style;
}

/**
 * One pet character. Renders the art-sprites strip for (species, activity) when available,
 * otherwise the built-in animated SVG. Never throws on missing assets.
 */
export function PetSprite(props: PetSpriteProps): JSX.Element {
  const { species, activity, size = 96, facing = "right", walking = false, skin, sprites, className, title, style } = props;
  usePetStyles();
  const anim = sprites ? resolveSpriteAnimation(sprites.manifest, species, activity, walking) : null;
  const url = anim && sprites ? spriteUrl(sprites.baseUrl, anim.src) : null;
  const status = useImageStatus(url);
  const strip = anim && url && status === "ok" ? stripStyle(anim, url, size) : null;
  const label = title ?? `${SPECIES_META[species]?.label ?? "Pet"}: ${activity}`;
  const cls = ["ap-pet", `ap-sp-${species}`, strip ? "ap-has-strip" : "", className ?? ""].filter(Boolean).join(" ");
  return (
    <div
      className={cls}
      data-activity={activity}
      data-facing={facing}
      data-walking={walking ? "true" : "false"}
      data-skin={skin || "default"}
      role="img"
      aria-label={label}
      style={{ width: size, height: size, ...style }}
    >
      <div className="ap-zoom">
        <div className="ap-shadow" />
        <div className="ap-zoom-flip">
          <div className="ap-flip">
            <div className="ap-move">{strip ? <div className="ap-strip" style={strip} /> : <SpeciesSvg species={species} />}</div>
          </div>
        </div>
      </div>
    </div>
  );
}
