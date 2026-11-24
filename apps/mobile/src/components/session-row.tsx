// One session in a list: agent + folder, status pill, last activity; opens the transcript.
import type { Agent, Session } from "@krypto/protocol";
import { useRouter } from "expo-router";
import { Pressable, Text, View } from "react-native";
import { Icon, Pill } from "@/components/ui";
import { baseName, sessionTitle, sourceLabel, timeAgo } from "@/lib/format";
import { STATUS_LABEL, statusColor, useTheme } from "@/theme";

export function SessionRow({ session, agents, dimmed }: { session: Session; agents: readonly Agent[]; dimmed?: boolean }) {
  const t = useTheme();
  const router = useRouter();
  const c = statusColor(t, session.status);
  const ended = session.endedAt != null;
  return (
    <Pressable
      onPress={() => router.push({ pathname: "/session/[id]", params: { id: session.id } })}
      accessibilityRole="button"
      accessibilityLabel={`${sessionTitle(session, agents)}, ${ended ? "ended" : STATUS_LABEL[session.status]}`}
      style={({ pressed }) => ({ opacity: dimmed ? 0.55 : pressed ? 0.7 : 1 })}
    >
      <View style={{ flexDirection: "row", alignItems: "center", gap: 12, paddingVertical: 10 }}>
        <View style={{ width: 36, height: 36, borderRadius: 10, backgroundColor: c.bg, alignItems: "center", justifyContent: "center" }}>
          <Icon name={session.source === "owned" ? "pawprint.fill" : "terminal.fill"} color={c.fg} size={17} />
        </View>
        <View style={{ flex: 1, gap: 2 }}>
          <Text style={{ color: t.text, fontSize: 16, fontWeight: "600" }} numberOfLines={1}>
            {sessionTitle(session, agents)}
          </Text>
          <Text style={{ color: t.textMuted, fontSize: 13 }} numberOfLines={1}>
            {baseName(session.cwd)} · {sourceLabel(session.source)} · {timeAgo(session.lastEventAt)}
          </Text>
        </View>
        <Pill text={ended ? "Ended" : STATUS_LABEL[session.status]} fg={ended ? t.textMuted : c.fg} bg={ended ? t.backgroundAlt : c.bg} />
      </View>
    </Pressable>
  );
}
