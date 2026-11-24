// Small cozy UI kit: cards, buttons, pills, rows, banners, empty states. Inline styles + the theme palette.
import * as Haptics from "expo-haptics";
import { Image } from "expo-image";
import type { ReactNode } from "react";
import { ActivityIndicator, Pressable, Text, View, type StyleProp, type TextStyle, type ViewStyle } from "react-native";
import Animated, { FadeIn, FadeOut } from "react-native-reanimated";
import { Fonts, Radius, useTheme, type Palette } from "@/theme";

export function tapHaptic(kind: "light" | "success" | "warning" | "error" = "light"): void {
  if (process.env.EXPO_OS !== "ios") return;
  if (kind === "light") void Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Light);
  else void Haptics.notificationAsync(
    kind === "success" ? Haptics.NotificationFeedbackType.Success : kind === "warning" ? Haptics.NotificationFeedbackType.Warning : Haptics.NotificationFeedbackType.Error,
  );
}

/** SF Symbol on iOS; a plain glyph elsewhere. */
const GLYPHS: Record<string, string> = {
  "checkmark.circle.fill": "✓",
  "xmark.circle.fill": "✕",
  "chevron.right": "›",
  "desktopcomputer": "▣",
  "laptopcomputer": "▭",
  "pawprint.fill": "🐾",
  "qrcode.viewfinder": "⌗",
  "bolt.horizontal.circle.fill": "⇄",
  "bell.badge.fill": "🔔",
  "lock.shield.fill": "🔒",
  "faceid": "◉",
  "plus.circle.fill": "+",
  "paperplane.fill": "➤",
  "stop.circle.fill": "■",
  "trash.fill": "🗑",
  "exclamationmark.triangle.fill": "!",
  "sparkles": "✦",
  "terminal.fill": "›_",
  "folder.fill": "▤",
  "antenna.radiowaves.left.and.right": "((·))",
};

export function Icon({ name, size = 18, color }: { name: string; size?: number; color: string }) {
  if (process.env.EXPO_OS === "ios") {
    return <Image source={`sf:${name}`} style={{ width: size, height: size }} tintColor={color} contentFit="contain" />;
  }
  return <Text style={{ fontSize: size * 0.9, color, lineHeight: size * 1.1, textAlign: "center", minWidth: size }}>{GLYPHS[name] ?? "•"}</Text>;
}

export function Card({ children, style, tone }: { children: ReactNode; style?: StyleProp<ViewStyle>; tone?: "alert" | "accent" }) {
  const t = useTheme();
  const border = tone === "alert" ? t.danger : tone === "accent" ? t.accent : t.border;
  return (
    <View
      style={[
        {
          backgroundColor: t.card,
          borderRadius: Radius.lg,
          borderCurve: "continuous",
          borderWidth: 1,
          borderColor: border,
          padding: 16,
          gap: 10,
          boxShadow: `0 2px 10px ${t.shadow}`,
        },
        style,
      ]}
    >
      {children}
    </View>
  );
}

type ButtonKind = "primary" | "secondary" | "danger" | "ghost";

function buttonColors(t: Palette, kind: ButtonKind, disabled: boolean): { bg: string; fg: string; border: string } {
  if (disabled) return { bg: t.backgroundAlt, fg: t.textFaint, border: t.border };
  switch (kind) {
    case "primary":
      return { bg: t.accent, fg: t.accentText, border: t.accent };
    case "danger":
      return { bg: t.dangerSoft, fg: t.danger, border: t.dangerSoft };
    case "ghost":
      return { bg: "transparent", fg: t.accent, border: "transparent" };
    default:
      return { bg: t.cardRaised, fg: t.text, border: t.border };
  }
}

export function Button({
  title,
  onPress,
  kind = "secondary",
  disabled = false,
  busy = false,
  icon,
  style,
  accessibilityHint,
}: {
  title: string;
  onPress: () => void;
  kind?: ButtonKind;
  disabled?: boolean;
  busy?: boolean;
  icon?: string;
  style?: StyleProp<ViewStyle>;
  accessibilityHint?: string;
}) {
  const t = useTheme();
  const c = buttonColors(t, kind, disabled);
  return (
    <Pressable
      onPress={() => {
        tapHaptic();
        onPress();
      }}
      disabled={disabled || busy}
      accessibilityRole="button"
      accessibilityLabel={title}
      accessibilityHint={accessibilityHint}
      accessibilityState={{ disabled: disabled || busy, busy }}
      style={({ pressed }) => [
        {
          minHeight: 48,
          paddingHorizontal: 18,
          borderRadius: Radius.pill,
          backgroundColor: c.bg,
          borderWidth: 1,
          borderColor: c.border,
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "center",
          gap: 8,
          opacity: pressed ? 0.75 : 1,
          transform: [{ scale: pressed ? 0.98 : 1 }],
        },
        style,
      ]}
    >
      {busy ? <ActivityIndicator color={c.fg} /> : icon ? <Icon name={icon} color={c.fg} size={17} /> : null}
      <Text style={{ color: c.fg, fontSize: 16, fontWeight: "700", fontFamily: Fonts.rounded }}>{title}</Text>
    </Pressable>
  );
}

export function Pill({ text, fg, bg, style }: { text: string; fg: string; bg: string; style?: StyleProp<ViewStyle> }) {
  return (
    <View style={[{ backgroundColor: bg, borderRadius: Radius.pill, paddingHorizontal: 10, paddingVertical: 4, alignSelf: "flex-start" }, style]}>
      <Text style={{ color: fg, fontSize: 12, fontWeight: "700", fontFamily: Fonts.rounded }}>{text}</Text>
    </View>
  );
}

export function Dot({ on, size = 9 }: { on: boolean; size?: number }) {
  const t = useTheme();
  return (
    <View
      accessibilityLabel={on ? "online" : "offline"}
      style={{ width: size, height: size, borderRadius: size / 2, backgroundColor: on ? t.success : t.textFaint }}
    />
  );
}

export function SectionTitle({ children, right }: { children: string; right?: ReactNode }) {
  const t = useTheme();
  return (
    <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between", paddingHorizontal: 4, marginTop: 6 }}>
      <Text style={{ color: t.textMuted, fontSize: 13, fontWeight: "700", letterSpacing: 0.6, textTransform: "uppercase" }}>{children}</Text>
      {right}
    </View>
  );
}

export function Title({ children, style }: { children: ReactNode; style?: StyleProp<TextStyle> }) {
  const t = useTheme();
  return <Text style={[{ color: t.text, fontSize: 20, fontWeight: "800", fontFamily: Fonts.rounded }, style]}>{children}</Text>;
}

export function Body({ children, muted, style, selectable }: { children: ReactNode; muted?: boolean; style?: StyleProp<TextStyle>; selectable?: boolean }) {
  const t = useTheme();
  return (
    <Text selectable={selectable} style={[{ color: muted ? t.textMuted : t.text, fontSize: 15, lineHeight: 21 }, style]}>
      {children}
    </Text>
  );
}

export function Row({
  title,
  subtitle,
  left,
  right,
  onPress,
  chevron,
}: {
  title: string;
  subtitle?: string;
  left?: ReactNode;
  right?: ReactNode;
  onPress?: () => void;
  chevron?: boolean;
}) {
  const t = useTheme();
  const inner = (
    <View style={{ flexDirection: "row", alignItems: "center", gap: 12, paddingVertical: 10 }}>
      {left}
      <View style={{ flex: 1, gap: 2 }}>
        <Text style={{ color: t.text, fontSize: 16, fontWeight: "600" }} numberOfLines={1}>
          {title}
        </Text>
        {subtitle ? (
          <Text style={{ color: t.textMuted, fontSize: 13 }} numberOfLines={2}>
            {subtitle}
          </Text>
        ) : null}
      </View>
      {right}
      {chevron ? <Icon name="chevron.right" color={t.textFaint} size={14} /> : null}
    </View>
  );
  if (!onPress) return inner;
  return (
    <Pressable onPress={onPress} accessibilityRole="button" accessibilityLabel={title} style={({ pressed }) => ({ opacity: pressed ? 0.6 : 1 })}>
      {inner}
    </Pressable>
  );
}

export function Banner({ tone, text, icon }: { tone: "info" | "warning" | "danger" | "success"; text: string; icon?: string }) {
  const t = useTheme();
  const c =
    tone === "danger"
      ? { fg: t.danger, bg: t.dangerSoft }
      : tone === "warning"
        ? { fg: t.warning, bg: t.warningSoft }
        : tone === "success"
          ? { fg: t.success, bg: t.successSoft }
          : { fg: t.info, bg: t.infoSoft };
  return (
    <Animated.View
      entering={FadeIn}
      exiting={FadeOut}
      style={{ flexDirection: "row", gap: 10, alignItems: "flex-start", backgroundColor: c.bg, padding: 12, borderRadius: Radius.md, borderCurve: "continuous" }}
    >
      {icon ? <Icon name={icon} color={c.fg} size={18} /> : null}
      <Text selectable style={{ flex: 1, color: c.fg, fontSize: 14, lineHeight: 20, fontWeight: "600" }}>
        {text}
      </Text>
    </Animated.View>
  );
}

export function Divider() {
  const t = useTheme();
  return <View style={{ height: 1, backgroundColor: t.border, marginVertical: 2 }} />;
}
