// Built-in react-native-svg pets: the fallback when a sprite strip is missing or fails to load.
// Simple, friendly faces; expression follows the activity.
import type { PetActivity } from "@krypto/pet-engine";
import type { Species } from "@krypto/protocol";
import Svg, { Circle, Ellipse, G, Path, Rect, Text as SvgText } from "react-native-svg";

const INK = "#3A2417";

interface Props {
  species: Species;
  activity: PetActivity;
  size: number;
}

function Eyes({ activity, cx = 50, cy = 52, gap = 13 }: { activity: PetActivity; cx?: number; cy?: number; gap?: number }) {
  if (activity === "napping") {
    return (
      <G stroke={INK} strokeWidth={2.6} strokeLinecap="round" fill="none">
        <Path d={`M${cx - gap - 5} ${cy} q5 4 10 0`} />
        <Path d={`M${cx + gap - 5} ${cy} q5 4 10 0`} />
      </G>
    );
  }
  if (activity === "happy") {
    return (
      <G stroke={INK} strokeWidth={2.6} strokeLinecap="round" fill="none">
        <Path d={`M${cx - gap - 5} ${cy + 2} q5 -6 10 0`} />
        <Path d={`M${cx + gap - 5} ${cy + 2} q5 -6 10 0`} />
      </G>
    );
  }
  const r = activity === "alert" ? 4.6 : 3.8;
  const dy = activity === "sad" ? 2 : 0;
  return (
    <G>
      <Circle cx={cx - gap} cy={cy + dy} r={r} fill={INK} />
      <Circle cx={cx + gap} cy={cy + dy} r={r} fill={INK} />
      <Circle cx={cx - gap + 1.3} cy={cy + dy - 1.4} r={1.2} fill="#fff" />
      <Circle cx={cx + gap + 1.3} cy={cy + dy - 1.4} r={1.2} fill="#fff" />
      {activity === "sad" ? <Path d={`M${cx + gap + 3} ${cy + 6} q2 5 0 7 q-2 -2 0 -7`} fill="#79A8EC" /> : null}
    </G>
  );
}

function Mouth({ activity, cx = 50, cy = 66 }: { activity: PetActivity; cx?: number; cy?: number }) {
  if (activity === "happy" || activity === "alert") {
    return <Path d={`M${cx - 7} ${cy - 1} q7 9 14 0 z`} fill="#E36B6B" stroke={INK} strokeWidth={2} strokeLinejoin="round" />;
  }
  if (activity === "sad") return <Path d={`M${cx - 6} ${cy + 3} q6 -6 12 0`} stroke={INK} strokeWidth={2.4} fill="none" strokeLinecap="round" />;
  return <Path d={`M${cx - 6} ${cy} q3 3 6 0 q3 3 6 0`} stroke={INK} strokeWidth={2.2} fill="none" strokeLinecap="round" />;
}

function Badge({ activity }: { activity: PetActivity }) {
  switch (activity) {
    case "alert":
      return (
        <G>
          <Circle cx={84} cy={16} r={11} fill="#C8372D" />
          <SvgText x={84} y={21} fontSize={15} fontWeight="bold" fill="#fff" textAnchor="middle">
            !
          </SvgText>
        </G>
      );
    case "thinking":
      return (
        <G>
          <Circle cx={84} cy={16} r={11} fill="#E1ECFA" stroke="#3B6FB6" strokeWidth={1.5} />
          <SvgText x={84} y={21} fontSize={14} fontWeight="bold" fill="#3B6FB6" textAnchor="middle">
            ?
          </SvgText>
        </G>
      );
    case "napping":
      return (
        <SvgText x={80} y={22} fontSize={14} fontWeight="bold" fill="#8D7560">
          z
        </SvgText>
      );
    case "planning":
      return (
        <G>
          <Rect x={74} y={6} width={18} height={22} rx={3} fill="#fff" stroke={INK} strokeWidth={1.5} />
          <Path d="M78 13 l2 2 l4 -4 M78 21 l2 2 l4 -4" stroke="#2F8F5B" strokeWidth={1.6} fill="none" />
        </G>
      );
    case "working":
      return <Path d="M76 8 l10 10 M80 6 l8 8 l-3 3 l-8 -8 z" stroke={INK} strokeWidth={2} fill="#F2B04F" />;
    case "happy":
      return <Path d="M84 10 c-3 -5 -10 -2 -6 4 l6 6 l6 -6 c4 -6 -3 -9 -6 -4 z" fill="#E36B6B" />;
    default:
      return null;
  }
}

function Dog({ activity }: { activity: PetActivity }) {
  const earDrop = activity === "sad" || activity === "napping" ? 6 : activity === "alert" || activity === "thinking" ? -4 : 0;
  return (
    <G>
      <Path d={`M22 ${34 + earDrop} q-12 6 -8 30 q4 8 12 2 q4 -18 8 -26 z`} fill="#C98A3E" stroke={INK} strokeWidth={2.4} />
      <Path d={`M78 ${34 + earDrop} q12 6 8 30 q-4 8 -12 2 q-4 -18 -8 -26 z`} fill="#C98A3E" stroke={INK} strokeWidth={2.4} />
      <Ellipse cx={50} cy={54} rx={30} ry={28} fill="#F2B04F" stroke={INK} strokeWidth={2.6} />
      <Ellipse cx={50} cy={66} rx={17} ry={12} fill="#FFF1DC" />
      <Eyes activity={activity} />
      <Ellipse cx={50} cy={60} rx={5} ry={3.6} fill={INK} />
      <Mouth activity={activity} cy={68} />
      <Circle cx={30} cy={64} r={4} fill="#F7A7A0" opacity={0.7} />
      <Circle cx={70} cy={64} r={4} fill="#F7A7A0" opacity={0.7} />
      <Path d="M30 80 q20 8 40 0" stroke="#C8372D" strokeWidth={5} fill="none" strokeLinecap="round" />
      <Circle cx={50} cy={86} r={3.6} fill="#F5C542" stroke={INK} strokeWidth={1.2} />
    </G>
  );
}

function Cat({ activity }: { activity: PetActivity }) {
  return (
    <G>
      <Path d="M24 44 l2 -26 l20 14 z" fill="#9AA3AF" stroke={INK} strokeWidth={2.4} strokeLinejoin="round" />
      <Path d="M76 44 l-2 -26 l-20 14 z" fill="#9AA3AF" stroke={INK} strokeWidth={2.4} strokeLinejoin="round" />
      <Ellipse cx={50} cy={56} rx={30} ry={26} fill="#B8C0CB" stroke={INK} strokeWidth={2.6} />
      <Path d="M40 34 l3 8 M50 32 v9 M60 34 l-3 8" stroke="#7D8794" strokeWidth={2.4} strokeLinecap="round" />
      <Eyes activity={activity} cy={54} />
      <Path d="M47 61 h6 l-3 3 z" fill="#E88A9A" />
      <Mouth activity={activity} cy={68} />
      <G stroke={INK} strokeWidth={1.4} strokeLinecap="round">
        <Path d="M30 62 h-12 M30 66 l-11 3" />
        <Path d="M70 62 h12 M70 66 l11 3" />
      </G>
    </G>
  );
}

function Bird({ activity }: { activity: PetActivity }) {
  return (
    <G>
      <Path d="M46 24 q4 -12 10 -6 q-2 4 -6 8 z" fill="#4FA3D9" stroke={INK} strokeWidth={2} />
      <Ellipse cx={50} cy={56} rx={28} ry={30} fill="#7CC3EE" stroke={INK} strokeWidth={2.6} />
      <Ellipse cx={50} cy={68} rx={16} ry={14} fill="#E8F6FD" />
      <Path d="M24 58 q-10 8 -4 18 q8 -4 10 -12 z" fill="#4FA3D9" stroke={INK} strokeWidth={2} />
      <Path d="M76 58 q10 8 4 18 q-8 -4 -10 -12 z" fill="#4FA3D9" stroke={INK} strokeWidth={2} />
      <Eyes activity={activity} cy={50} gap={11} />
      <Path d={activity === "alert" || activity === "happy" ? "M43 58 l7 -4 l7 4 l-7 6 z" : "M43 58 l7 -3 l7 3 l-7 4 z"} fill="#F5A623" stroke={INK} strokeWidth={1.8} strokeLinejoin="round" />
    </G>
  );
}

function Robot({ activity }: { activity: PetActivity }) {
  const lit = activity === "alert" ? "#C8372D" : activity === "happy" ? "#2F8F5B" : activity === "napping" ? "#8D7560" : "#3B6FB6";
  return (
    <G>
      <Path d="M50 26 v-10" stroke={INK} strokeWidth={2.4} />
      <Circle cx={50} cy={14} r={4.5} fill={lit} stroke={INK} strokeWidth={2} />
      <Rect x={20} y={26} width={60} height={56} rx={14} fill="#C9D3DE" stroke={INK} strokeWidth={2.6} />
      <Rect x={28} y={38} width={44} height={28} rx={8} fill="#26323F" />
      {activity === "napping" ? (
        <G stroke="#79A8EC" strokeWidth={2.6} strokeLinecap="round">
          <Path d="M36 52 h8 M56 52 h8" />
        </G>
      ) : (
        <G fill={lit === "#8D7560" ? "#79A8EC" : activity === "sad" ? "#E9B35A" : "#79E0F0"}>
          <Rect x={35} y={46} width={9} height={activity === "happy" ? 4 : 9} rx={2} />
          <Rect x={56} y={46} width={9} height={activity === "happy" ? 4 : 9} rx={2} />
        </G>
      )}
      <Rect x={38} y={71} width={24} height={4} rx={2} fill={INK} opacity={0.6} />
      <Rect x={12} y={46} width={8} height={16} rx={3} fill="#9FAAB6" stroke={INK} strokeWidth={2} />
      <Rect x={80} y={46} width={8} height={16} rx={3} fill="#9FAAB6" stroke={INK} strokeWidth={2} />
    </G>
  );
}

export function PetSvg({ species, activity, size }: Props) {
  return (
    <Svg width={size} height={size} viewBox="0 0 100 100" accessibilityElementsHidden importantForAccessibility="no-hide-descendants">
      <Ellipse cx={50} cy={95} rx={26} ry={4} fill="#000" opacity={0.08} />
      {species === "cat" ? (
        <Cat activity={activity} />
      ) : species === "bird" ? (
        <Bird activity={activity} />
      ) : species === "robot" ? (
        <Robot activity={activity} />
      ) : (
        <Dog activity={activity} />
      )}
      <Badge activity={activity} />
    </Svg>
  );
}
