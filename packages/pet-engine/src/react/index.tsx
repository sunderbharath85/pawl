// @pawl/pet-engine/react — pure SVG + CSS keyframes pets (sprite sheets optional), no other assets needed.
export { PetSprite, type PetSpriteProps } from "./PetSprite";
export { PetBubble, type PetBubbleProps } from "./PetBubble";
export { PetStage, type PetStageProps } from "./PetStage";
export { usePetStyles, usePetViews, usePrefersReducedMotion, type PetViewsSource } from "./hooks";
export { configurePetStyles, ensurePetStyles, PET_CSS } from "./styles";
export { SpeciesSvg } from "./art";
export { bubblePlacement, computeLanes, layoutVisitors, MAX_VISITORS, VISITOR_SCALE, type Lane, type VisitorLayout } from "./stage-logic";
