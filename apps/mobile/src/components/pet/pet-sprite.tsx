// Animated pet: plays the art-sprites PNG strip (copied by scripts/copy-pets.ts) with a stepped,
// native-driven translateX; falls back to the react-native-svg pet when the strip is missing or fails.
import type { PetActivity, SpriteAnimationName } from "@ai-pet/pet-engine";
import type { Species } from "@ai-pet/protocol";
import { Image } from "expo-image";
import { useEffect, useMemo, useState } from "react";
import { Animated, Easing, View, type StyleProp, type ViewStyle } from "react-native";
import { useReducedMotion } from "react-native-reanimated";
import { PET_SPRITES, type BundledSpriteAnimation } from "@/generated/pet-sprites";
import { PetSvg } from "./pet-svg";

export interface PetSpriteProps {
  species: Species;
  activity: PetActivity;
  size?: number;
  walking?: boolean;
  /** Mirror horizontally (strips face right). */
  facing?: "left" | "right";
  style?: StyleProp<ViewStyle>;
  /** Freeze on the first frame (lists with many pets, reduced motion). */
  still?: boolean;
}

export function resolveBundledSprite(species: Species, activity: PetActivity, walking = false): BundledSpriteAnimation | null {
  const sp = PET_SPRITES[species];
  if (!sp) return null;
  const name: SpriteAnimationName = walking && sp.walk ? "walk" : activity;
  return sp[name] ?? sp[activity] ?? null;
}

function stepRange(frames: number, size: number): { inputRange: number[]; outputRange: number[] } {
  const inputRange: number[] = [];
  const outputRange: number[] = [];
  for (let i = 0; i < frames; i++) {
    inputRange.push(i, i + 1);
    outputRange.push(-i * size, -i * size);
  }
  return { inputRange, outputRange };
}

function Strip({ anim, size, still, onError }: { anim: BundledSpriteAnimation; size: number; still: boolean; onError: () => void }) {
  const progress = useMemo(() => new Animated.Value(0), []);
  const frames = Math.max(1, anim.frames);

  useEffect(() => {
    progress.setValue(0);
    if (still || frames < 2) return;
    const duration = (frames * 1000) / Math.max(1, anim.fps);
    const timing = Animated.timing(progress, {
      toValue: frames,
      duration,
      easing: Easing.linear,
      useNativeDriver: true,
      isInteraction: false,
    });
    const run = anim.loop ? Animated.loop(timing) : timing;
    run.start();
    return () => run.stop();
  }, [progress, frames, anim.fps, anim.loop, still]);

  const translateX = useMemo(() => {
    if (frames < 2) return 0;
    const { inputRange, outputRange } = stepRange(frames, size);
    return progress.interpolate({ inputRange, outputRange, extrapolate: "clamp" });
  }, [progress, frames, size]);

  return (
    <View style={{ width: size, height: size, overflow: "hidden" }}>
      <Animated.View style={{ width: size * frames, height: size, transform: [{ translateX }] }}>
        <Image
          source={anim.source}
          style={{ width: size * frames, height: size }}
          contentFit="fill"
          cachePolicy="memory"
          transition={0}
          onError={onError}
          accessible={false}
        />
      </Animated.View>
    </View>
  );
}

export function PetSprite({ species, activity, size = 96, walking = false, facing = "right", style, still = false }: PetSpriteProps) {
  const reduceMotion = useReducedMotion();
  const anim = resolveBundledSprite(species, activity, walking);
  const key = `${species}/${walking ? "walk" : activity}`;
  const [failed, setFailed] = useState<string | null>(null);

  return (
    <View
      style={[{ width: size, height: size, transform: facing === "left" ? [{ scaleX: -1 }] : undefined }, style]}
      accessible
      accessibilityRole="image"
      accessibilityLabel={`${species}, ${activity}`}
    >
      {anim && failed !== key ? (
        <Strip key={key} anim={anim} size={size} still={still || reduceMotion} onError={() => setFailed(key)} />
      ) : (
        <PetSvg species={species} activity={activity} size={size} />
      )}
    </View>
  );
}
