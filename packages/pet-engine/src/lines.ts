// Flavor text per species/activity + species metadata. Pure data.
import type { Species } from "@krypto/protocol";
import type { PetActivity } from "./activity";

export const SPECIES_META: Record<Species, { label: string; emoji: string; sound: string }> = {
  dog: { label: "Dog", emoji: "🐕", sound: "Woof!" },
  cat: { label: "Cat", emoji: "🐈", sound: "Meow!" },
  bird: { label: "Bird", emoji: "🐦", sound: "Tweet!" },
  robot: { label: "Robot", emoji: "🤖", sound: "Beep boop!" },
};

export const SPECIES_LIST: readonly Species[] = ["dog", "cat", "bird", "robot"];

export const PET_LINES: Record<Species, Record<PetActivity, readonly string[]>> = {
  dog: {
    napping: ["Zzz… *paws twitch*", "*snores softly in the dog bed*", "Dreaming of tennis balls…"],
    idle: ["*sniffs around*", "Ready when you are!", "*wags tail lazily*", "Any sticks need fetching?"],
    thinking: ["*tilts head* Hmm?", "Ears up — thinking!", "*sniff sniff* Following a scent…"],
    working: ["*digs furiously*", "Fetching files!", "Dig dig dig!", "On it! *paws flying*"],
    planning: ["Got the checklist!", "*carries the plan proudly*", "Step one: sniff. Step two: dig."],
    alert: ["Woof! Need you!", "Woof woof! Over here!", "Bark! Can I do this?"],
    happy: ["Done! *wag wag wag*", "Zoomies! All done!", "Good job, team! *tail blur*"],
    sad: ["*whimpers* Something broke.", "Aww… that didn't work.", "*droopy ears* Uh oh."],
  },
  cat: {
    napping: ["*curled up* Zzz…", "*purrs in its sleep*", "Do not disturb. Napping."],
    idle: ["*grooms a paw*", "*stares at nothing in particular*", "I suppose I'm available."],
    thinking: ["*ears perk* Hmm…", "*tail flicks* Considering…", "Pondering. Do not rush me."],
    working: ["*kneads the keyboard*", "*bats at the code*", "Busy. Very busy."],
    planning: ["*sits on the to-do list*", "I have a plan. Obviously.", "Plotting… I mean planning."],
    alert: ["Meow! Human! Now!", "Mrrrow? Need you!", "*paws at you* Approve this?"],
    happy: ["*purrs loudly* Done.", "Nailed it. *slow blink*", "*happy tail* Finished!"],
    sad: ["*hisses at the error*", "Hmph. That failed.", "*flattened ears* Not good."],
  },
  bird: {
    napping: ["*head tucked* Zzz…", "*fluffed up, dozing*", "Tweet… zzz…"],
    idle: ["*preens feathers*", "Tweet tweet!", "*hops along*"],
    thinking: ["*cocks head* Chirp?", "Hmm… *peck peck*", "Thinking! Tweet!"],
    working: ["*pecks busily*", "Building the nest!", "Chirp chirp, working!"],
    planning: ["*carries the list*", "Flight plan ready!", "Plan in beak!"],
    alert: ["Squawk! Need you!", "Tweet! Tweet! Over here!", "*flaps wildly* Approve?"],
    happy: ["*sings a little song*", "Done! *happy flutter*", "Tweet-tweet-hooray!"],
    sad: ["*droopy feathers*", "Squawk… it broke.", "*sad chirp*"],
  },
  robot: {
    napping: ["Sleep mode. Zzz…", "*low-power hum*", "Standby… recharging."],
    idle: ["Awaiting instructions.", "*blinks LEDs*", "Systems nominal."],
    thinking: ["Processing… beep.", "*antenna twitches* Computing.", "Analyzing input…"],
    working: ["Executing task!", "*whirr click whirr*", "Beep boop, working!"],
    planning: ["Plan loaded: 100%.", "Generating checklist…", "Sequence planned."],
    alert: ["BEEP! Human input required!", "Alert! Approval needed!", "*flashing red* Need you!"],
    happy: ["Task complete! Beep!", "*happy beeps*", "Success! Doing a little dance."],
    sad: ["Error. *sad beep*", "Malfunction detected.", "*antenna droops* Failed."],
  },
};

/** Flavor text, e.g. petLine("dog","alert") → "Woof! Need you!". Deterministic for a given seed (default 0). */
export function petLine(species: Species, activity: PetActivity, seed = 0): string {
  const lines = (PET_LINES[species] ?? PET_LINES.dog)[activity] ?? PET_LINES.dog.idle;
  const n = Number.isFinite(seed) ? Math.abs(Math.floor(seed)) : 0;
  return lines[n % lines.length]!;
}

/** Small stable string hash (FNV-1a, 32-bit) for seeding lines per pet. */
export function hashString(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}
