// Cozy palette pulled from the dog sprite: honey fur, cocoa outline, cream belly, red collar.
import type { PetActivity } from "@ai-pet/pet-engine";
import type { SessionStatus } from "@ai-pet/protocol";
import { Platform, useColorScheme } from "react-native";

export interface Palette {
  scheme: "light" | "dark";
  background: string;
  backgroundAlt: string;
  card: string;
  cardRaised: string;
  border: string;
  text: string;
  textMuted: string;
  textFaint: string;
  accent: string;
  accentText: string;
  accentSoft: string;
  honey: string;
  collar: string;
  success: string;
  successSoft: string;
  danger: string;
  dangerSoft: string;
  warning: string;
  warningSoft: string;
  info: string;
  infoSoft: string;
  bubble: string;
  bubbleUser: string;
  bubbleUserText: string;
  shadow: string;
}

export const LIGHT: Palette = {
  scheme: "light",
  background: "#FFF6EA",
  backgroundAlt: "#FCEBD6",
  card: "#FFFFFF",
  cardRaised: "#FFFDF9",
  border: "#F0DEC6",
  text: "#3A2417",
  textMuted: "#7A5C46",
  textFaint: "#AD927B",
  accent: "#E07B2E",
  accentText: "#FFFFFF",
  accentSoft: "#FCE3CC",
  honey: "#F2B04F",
  collar: "#C8372D",
  success: "#2F8F5B",
  successSoft: "#DDF2E4",
  danger: "#C8372D",
  dangerSoft: "#FBE0DC",
  warning: "#B7791F",
  warningSoft: "#FDF0D5",
  info: "#3B6FB6",
  infoSoft: "#E1ECFA",
  bubble: "#FFFFFF",
  bubbleUser: "#E07B2E",
  bubbleUserText: "#FFFFFF",
  shadow: "rgba(90, 55, 25, 0.10)",
};

export const DARK: Palette = {
  scheme: "dark",
  background: "#17110C",
  backgroundAlt: "#1F1711",
  card: "#251C15",
  cardRaised: "#2D2219",
  border: "#3A2C20",
  text: "#F8ECDD",
  textMuted: "#CDB39A",
  textFaint: "#8D7560",
  accent: "#F29B4B",
  accentText: "#21150C",
  accentSoft: "#4A2F1A",
  honey: "#F5BC62",
  collar: "#E2574B",
  success: "#5BC48A",
  successSoft: "#173A26",
  danger: "#F06A5C",
  dangerSoft: "#45201B",
  warning: "#E9B35A",
  warningSoft: "#3D2D12",
  info: "#79A8EC",
  infoSoft: "#1B2B42",
  bubble: "#2D2219",
  bubbleUser: "#F29B4B",
  bubbleUserText: "#21150C",
  shadow: "rgba(0, 0, 0, 0.35)",
};

export function useTheme(): Palette {
  return useColorScheme() === "dark" ? DARK : LIGHT;
}

export const Fonts = {
  rounded: Platform.select({ ios: "ui-rounded", default: undefined }),
  mono: Platform.select({ ios: "Menlo", default: "monospace" }),
};

export const Radius = { sm: 10, md: 16, lg: 22, xl: 28, pill: 999 } as const;

export const ACTIVITY_LABEL: Record<PetActivity, string> = {
  napping: "Napping",
  idle: "Hanging out",
  thinking: "Thinking",
  working: "Digging in",
  planning: "Planning",
  alert: "Needs you",
  happy: "All done!",
  sad: "Uh oh",
};

export function activityColor(p: Palette, a: PetActivity): { fg: string; bg: string } {
  switch (a) {
    case "alert":
      return { fg: p.danger, bg: p.dangerSoft };
    case "happy":
      return { fg: p.success, bg: p.successSoft };
    case "sad":
      return { fg: p.warning, bg: p.warningSoft };
    case "thinking":
    case "planning":
      return { fg: p.info, bg: p.infoSoft };
    case "working":
      return { fg: p.accent, bg: p.accentSoft };
    default:
      return { fg: p.textMuted, bg: p.backgroundAlt };
  }
}

export const STATUS_LABEL: Record<SessionStatus, string> = {
  idle: "Idle",
  thinking: "Thinking",
  working: "Working",
  waiting_permission: "Waiting for you",
  done: "Done",
  error: "Error",
};

export function statusColor(p: Palette, s: SessionStatus): { fg: string; bg: string } {
  switch (s) {
    case "waiting_permission":
      return { fg: p.danger, bg: p.dangerSoft };
    case "done":
      return { fg: p.success, bg: p.successSoft };
    case "error":
      return { fg: p.warning, bg: p.warningSoft };
    case "thinking":
      return { fg: p.info, bg: p.infoSoft };
    case "working":
      return { fg: p.accent, bg: p.accentSoft };
    default:
      return { fg: p.textMuted, bg: p.backgroundAlt };
  }
}
