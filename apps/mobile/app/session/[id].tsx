// A session: status, the read-only transcript (from the mirror), its pending approvals, a prompt box (owned sessions
// only: hook sessions can't take prompts) and cancel. Everything acts through intents to the owning computer.
import { describePermission } from "@pawl/pet-engine";
import { Stack, useLocalSearchParams, useRouter } from "expo-router";
import { useMemo, useState } from "react";
import { KeyboardAvoidingView, ScrollView, Text, TextInput, View } from "react-native";
import Animated, { FadeIn } from "react-native-reanimated";
import { PetSprite } from "@/components/pet/pet-sprite";
import { Banner, Body, Button, Card, Icon, Pill } from "@/components/ui";
import { buildFeed, stopReasonLabel, type FeedItem } from "@/lib/feed";
import { clockTime, isBusy, sessionTitle, sourceLabel, truncate } from "@/lib/format";
import { decisionText, useActions } from "@/lib/phone/actions";
import { usePhone } from "@/lib/phone/phone-provider";
import { Fonts, Radius, STATUS_LABEL, statusColor, useTheme } from "@/theme";

const PROMPT_MAX = 16_000;

function FeedLine({ item, machine }: { item: FeedItem; machine: string }) {
  const t = useTheme();
  switch (item.kind) {
    case "user":
      return (
        <View style={{ alignSelf: "flex-end", maxWidth: "88%", backgroundColor: t.bubbleUser, borderRadius: Radius.md, borderCurve: "continuous", padding: 10 }}>
          <Text selectable style={{ color: t.bubbleUserText, fontSize: 15, lineHeight: 21 }}>
            {item.text}
          </Text>
        </View>
      );
    case "agent":
      return (
        <View style={{ alignSelf: "flex-start", maxWidth: "94%", backgroundColor: t.card, borderColor: t.border, borderWidth: 1, borderRadius: Radius.md, borderCurve: "continuous", padding: 10 }}>
          <Text selectable style={{ color: t.text, fontSize: 15, lineHeight: 21 }}>
            {item.text}
          </Text>
        </View>
      );
    case "thought":
      return (
        <Text selectable style={{ color: t.textFaint, fontSize: 13, fontStyle: "italic", lineHeight: 19 }}>
          {truncate(item.text, 600)}
        </Text>
      );
    case "tool": {
      const failed = item.status === "failed";
      const done = item.status === "completed";
      return (
        <View style={{ flexDirection: "row", gap: 8, alignItems: "center" }}>
          <Icon name={done ? "checkmark.circle.fill" : failed ? "xmark.circle.fill" : "terminal.fill"} color={failed ? t.danger : done ? t.success : t.accent} size={15} />
          <Text selectable style={{ color: t.textMuted, fontSize: 14, flex: 1, fontFamily: Fonts.mono }} numberOfLines={2}>
            {item.title}
          </Text>
        </View>
      );
    }
    case "plan":
      return (
        <View style={{ gap: 4, backgroundColor: t.infoSoft, borderRadius: Radius.sm, padding: 10 }}>
          {item.entries.map((e, i) => (
            <Text key={i} style={{ color: t.info, fontSize: 14 }}>
              {e.status === "completed" ? "☑" : e.status === "in_progress" ? "◐" : "☐"} {e.content}
            </Text>
          ))}
        </View>
      );
    case "permission":
      return (
        <Text style={{ color: item.outcome ? t.textMuted : t.danger, fontSize: 14, fontWeight: "600" }}>
          {item.outcome ? "✓ " : "⏳ "}
          {item.title}
          {item.by ? ` — ${item.by}` : ""}
        </Text>
      );
    case "stop":
      return <Text style={{ color: t.success, fontSize: 13, fontWeight: "700", textAlign: "center" }}>{stopReasonLabel(String(item.reason))}</Text>;
    case "error":
      return <Banner tone="danger" text={item.message} />;
    case "system":
      return <Text style={{ color: item.tone === "alert" ? t.danger : t.textFaint, fontSize: 13, textAlign: "center" }}>{item.text.replace("its computer", machine)}</Text>;
  }
}

export default function SessionScreen() {
  const t = useTheme();
  const router = useRouter();
  const { id } = useLocalSearchParams<{ id: string }>();
  const { snapshot, node } = usePhone();
  const actions = useActions();
  const [text, setText] = useState("");
  const [busy, setBusy] = useState<"prompt" | "cancel" | null>(null);
  const [note, setNote] = useState<{ tone: "success" | "danger"; text: string } | null>(null);
  const session = snapshot.sessions[id];
  const events = snapshot.events[id];
  const feed = useMemo(() => buildFeed(events ?? []).slice(-200), [events]);

  if (!session) {
    return (
      <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={{ padding: 16, gap: 12 }}>
        <Stack.Screen options={{ title: "Session" }} />
        <Banner tone="info" text="This session isn't in the phone's copy any more (it may have ended a while ago)." />
      </ScrollView>
    );
  }

  const machineId = session.machineId;
  const machine = snapshot.machines[machineId];
  const machineLabel = machine?.name ?? "its computer";
  const online = machine?.online === true;
  const agents = snapshot.agents[machineId] ?? [];
  const pet = Object.values(snapshot.pets).find((p) => p.machineId === machineId && p.agentId === session.agentId);
  const c = statusColor(t, session.status);
  const ended = session.endedAt != null;
  const canPrompt = session.source === "owned" && !ended;
  const pending = Object.values(snapshot.pending).filter((p) => p.sessionId === session.id);
  const decided = snapshot.resolutions.filter((r) => r.sessionId === session.id).slice(0, 3);

  const send = async () => {
    const body = text.trim();
    if (!body) return;
    setBusy("prompt");
    setNote(null);
    const res = await actions.prompt(session.id, machineId, body.slice(0, PROMPT_MAX));
    setBusy(null);
    if (res.ok) {
      setText("");
      setNote({ tone: "success", text: `Sent to ${machineLabel}.` });
    } else setNote({ tone: "danger", text: res.message });
  };

  const stop = async () => {
    setBusy("cancel");
    setNote(null);
    const res = await actions.cancel(session.id, machineId);
    setBusy(null);
    setNote(res.ok ? { tone: "success", text: "Asked the agent to stop." } : { tone: "danger", text: res.message });
  };

  return (
    <KeyboardAvoidingView behavior={process.env.EXPO_OS === "ios" ? "padding" : undefined} style={{ flex: 1 }} keyboardVerticalOffset={90}>
      <Stack.Screen options={{ title: sessionTitle(session, agents) }} />
      <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={{ padding: 16, gap: 12, paddingBottom: 32 }} keyboardDismissMode="interactive">
        <Card>
          <View style={{ flexDirection: "row", alignItems: "center", gap: 12 }}>
            <PetSprite species={pet?.species ?? "dog"} activity={ended ? "napping" : session.status === "waiting_permission" ? "alert" : session.status === "working" ? "working" : session.status === "thinking" ? "thinking" : session.status === "error" ? "sad" : "idle"} size={60} />
            <View style={{ flex: 1, gap: 4 }}>
              <Text style={{ color: t.text, fontSize: 17, fontWeight: "800" }} numberOfLines={1}>
                {pet?.name ?? "Agent"} on {machineLabel}
              </Text>
              <Text selectable style={{ color: t.textMuted, fontSize: 13 }} numberOfLines={2}>
                {session.cwd}
              </Text>
              <View style={{ flexDirection: "row", gap: 8 }}>
                <Pill text={ended ? "Ended" : STATUS_LABEL[session.status]} fg={c.fg} bg={c.bg} />
                <Pill text={sourceLabel(session.source)} fg={t.textMuted} bg={t.backgroundAlt} />
              </View>
            </View>
          </View>
          {!online ? <Banner tone="info" text={`${machineLabel} is offline. This is what it last sent.`} /> : null}
        </Card>

        {pending.map((r) => (
          <Card key={r.requestId} tone="alert">
            <Body style={{ fontWeight: "700" }}>Waiting for you: {describePermission(r)}</Body>
            <Button title="Review in Approvals" kind="primary" onPress={() => router.navigate("/approvals")} />
          </Card>
        ))}
        {decided.map((r) => (
          <Text key={r.requestId} style={{ color: t.textMuted, fontSize: 13 }}>
            {r.request ? `${describePermission(r.request)}: ` : ""}
            {decisionText(snapshot, r, (x) => node.deviceName(x))}
          </Text>
        ))}

        <View style={{ gap: 10 }}>
          {feed.length === 0 ? <Body muted>No activity yet.</Body> : null}
          {feed.map((item) => (
            <Animated.View key={item.key} entering={FadeIn}>
              <FeedLine item={item} machine={machineLabel} />
            </Animated.View>
          ))}
          {feed.length > 0 ? <Text style={{ color: t.textFaint, fontSize: 12, textAlign: "center" }}>Last update {clockTime(session.lastEventAt)}</Text> : null}
        </View>

        {note ? <Banner tone={note.tone} text={note.text} /> : null}

        {canPrompt ? (
          <Card>
            <TextInput
              value={text}
              onChangeText={setText}
              placeholder={`Tell ${pet?.name ?? "the agent"} what to do next…`}
              placeholderTextColor={t.textFaint}
              multiline
              maxLength={PROMPT_MAX}
              editable={online && busy === null}
              accessibilityLabel="Prompt"
              style={{ color: t.text, fontSize: 16, minHeight: 64, maxHeight: 180, textAlignVertical: "top" }}
            />
            <View style={{ flexDirection: "row", gap: 10 }}>
              {isBusy(session) ? (
                <Button title="Stop" kind="danger" icon="stop.circle.fill" onPress={() => void stop()} busy={busy === "cancel"} disabled={!online || busy !== null} style={{ flex: 1 }} />
              ) : null}
              <Button title="Send" kind="primary" icon="paperplane.fill" onPress={() => void send()} busy={busy === "prompt"} disabled={!online || busy !== null || !text.trim()} style={{ flex: 1.4 }} />
            </View>
          </Card>
        ) : !ended ? (
          <View style={{ gap: 8 }}>
            <Body muted style={{ fontSize: 13 }}>
              This session runs in {sourceLabel(session.source)}, so it takes prompts there, not from Pawl.
            </Body>
            {isBusy(session) ? <Button title="Stop" kind="danger" icon="stop.circle.fill" onPress={() => void stop()} busy={busy === "cancel"} disabled={!online || busy !== null} /> : null}
          </View>
        ) : null}
      </ScrollView>
    </KeyboardAvoidingView>
  );
}
