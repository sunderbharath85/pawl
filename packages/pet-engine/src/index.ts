// @ai-pet/pet-engine — pure pet logic (no React, no DOM). React components: "@ai-pet/pet-engine/react".
export {
  activityForEvent,
  activityForStatus,
  HAPPY_DECAY_MS,
  lastSignificantEvents,
  NAP_AFTER_MS,
  PET_ACTIVITIES,
  SAD_DECAY_MS,
  type PetActivity,
} from "./activity";
export { hashString, PET_LINES, petLine, SPECIES_LIST, SPECIES_META } from "./lines";
export { derivePetViews, describePermission, nextPetViewChange, type DerivePetViewsInput, type PetBubbleData, type PetView } from "./views";
export {
  clearSpriteManifestCache,
  loadSpriteManifest,
  parseSpriteManifest,
  resolveSpriteAnimation,
  SPRITE_ANIMATION_NAMES,
  spriteUrl,
  type PetSpritesConfig,
  type SpriteAnimation,
  type SpriteAnimationName,
  type SpriteManifest,
  type SpriteSpecies,
} from "./sprites";
