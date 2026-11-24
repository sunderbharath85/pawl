// Start: machine -> repo (that computer's repo index) -> agent (installed there) -> optional first prompt -> confirm
// with biometrics -> start intent. The computer only starts agents in folders it already knows.
import type { Agent, RepoRef } from "@pawl/protocol";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useMemo, useState, type ReactNode } from "react";
import { Pressable, ScrollView, Text, TextInput, View } from "react-native";
import Animated, { FadeIn } from "react-native-reanimated";
import { Banner, Body, Button, Card, Dot, Icon, SectionTitle } from "@/components/ui";
import { agentName, shortPath, timeAgo } from "@/lib/format";
import { useActions, useGateAvailability } from "@/lib/phone/actions";
import { usePhone } from "@/lib/phone/phone-provider";
import { Fonts, Radius, useTheme } from "@/theme";

function Choice({ selected, title, subtitle, onPress, disabled, left }: { selected: boolean; title: string; subtitle?: string; onPress: () => void; disabled?: boolean; left?: ReactNode }) {
  const t = useTheme();
  return (
    <Pressable
      onPress={onPress}
      disabled={disabled}
      accessibilityRole="radio"
      accessibilityState={{ selected, disabled }}
      accessibilityLabel={title}
      style={({ pressed }) => ({
        flexDirection: "row",
        alignItems: "center",
        gap: 12,
        padding: 12,
        borderRadius: Radius.md,
        borderCurve: "continuous",
        borderWidth: 2,
        borderColor: selected ? t.accent : t.border,
        backgroundColor: selected ? t.accentSoft : t.card,
        opacity: disabled ? 0.45 : pressed ? 0.8 : 1,
      })}
    >
      {left}
      <View style={{ flex: 1, gap: 2 }}>
        <Text style={{ color: t.text, fontSize: 16, fontWeight: "700" }} numberOfLines={1}>
          {title}
        </Text>
        {subtitle ? (
          <Text style={{ color: t.textMuted, fontSize: 13 }} numberOfLines={1}>
            {subtitle}
          </Text>
        ) : null}
      </View>
      {selected ? <Icon name="checkmark.circle.fill" color={t.accent} size={20} /> : null}
    </Pressable>
  );
}

export default function StartScreen() {
  const t = useTheme();
  const router = useRouter();
  const params = useLocalSearchParams<{ machine?: string }>();
  const { snapshot } = usePhone();
  const actions = useActions();
  const { availability } = useGateAvailability();
  const online = snapshot.hubs.filter((h) => h.online);
  const [machineId, setMachineId] = useState<string | null>(() => params.machine ?? (online.length === 1 ? online[0]!.id : null));
  const [repo, setRepo] = useState<RepoRef | null>(null);
  const [agent, setAgent] = useState<Agent | null>(null);
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const repos = useMemo(() => (machineId ? (snapshot.repos[machineId] ?? []) : []), [snapshot.repos, machineId]);
  const agents = useMemo(() => (machineId ? (snapshot.agents[machineId] ?? []).filter((a) => a.installed) : []), [snapshot.agents, machineId]);
  const machine = machineId ? snapshot.machines[machineId] : undefined;
  const pick = (id: string) => {
    setMachineId(id);
    setRepo(null);
    setAgent(null);
    setError(null);
  };
  const chosenAgent = agent ?? (agents.length === 1 ? agents[0]! : null);
  const ready = machine?.online === true && repo !== null && chosenAgent !== null && availability?.available === true;

  const go = async () => {
    if (!machineId || !repo || !chosenAgent) return;
    setBusy(true);
    setError(null);
    const res = await actions.start({
      machineId,
      agentId: chosenAgent.id,
      agentName: chosenAgent.name || agentName(agents, chosenAgent.id),
      cwd: repo.path,
      repoName: repo.name,
      prompt: prompt.trim() || undefined,
    });
    setBusy(false);
    if (!res.ok) {
      if (!res.cancelled) setError(res.message);
      return;
    }
    const session = (res.applied.result as { session?: { id?: string } } | undefined)?.session;
    if (session?.id) router.replace({ pathname: "/session/[id]", params: { id: session.id } });
    else router.back();
  };

  return (
    <ScrollView contentInsetAdjustmentBehavior="automatic" keyboardShouldPersistTaps="handled" contentContainerStyle={{ padding: 16, gap: 12, paddingBottom: 48 }}>
      {availability && !availability.available ? <Banner tone="warning" icon="lock.shield.fill" text={`Starting agents is off on this phone. ${availability.explanation}`} /> : null}
      {error ? <Banner tone="danger" icon="exclamationmark.triangle.fill" text={error} /> : null}

      <SectionTitle>1 · Computer</SectionTitle>
      {snapshot.hubs.length === 0 ? <Body muted>Pair a computer first.</Body> : null}
      {snapshot.hubs.map((h) => (
        <Choice
          key={h.id}
          selected={machineId === h.id}
          title={h.name}
          subtitle={h.online ? "Online" : h.lastSeen ? `Offline · seen ${timeAgo(h.lastSeen)} ago` : "Offline"}
          onPress={() => pick(h.id)}
          disabled={!h.online}
          left={<Dot on={h.online} />}
        />
      ))}

      {machineId ? (
        <Animated.View entering={FadeIn} style={{ gap: 12 }}>
          <SectionTitle>2 · Repo</SectionTitle>
          {repos.length === 0 ? (
            <Body muted>
              {machine?.name ?? "That computer"} hasn't shared any repos yet. It lists folders its agents worked in and the ones in its Settings → Repos.
            </Body>
          ) : null}
          {repos.slice(0, 30).map((r) => (
            <Choice key={r.path} selected={repo?.path === r.path} title={r.name} subtitle={shortPath(r.path)} onPress={() => setRepo(r)} left={<Icon name="folder.fill" color={t.honey} size={20} />} />
          ))}

          <SectionTitle>3 · Agent</SectionTitle>
          {agents.length === 0 ? <Body muted>No agents installed on {machine?.name ?? "that computer"} (or it hasn't said yet).</Body> : null}
          {agents.map((a) => (
            <Choice key={a.id} selected={chosenAgent?.id === a.id} title={a.name || agentName(agents, a.id)} subtitle={a.version ? `v${a.version}` : undefined} onPress={() => setAgent(a)} left={<Icon name="sparkles" color={t.accent} size={18} />} />
          ))}

          <SectionTitle>First message (optional)</SectionTitle>
          <Card>
            <TextInput
              value={prompt}
              onChangeText={setPrompt}
              placeholder="Run the tests and fix what fails"
              placeholderTextColor={t.textFaint}
              multiline
              maxLength={16_000}
              accessibilityLabel="First message"
              style={{ color: t.text, fontSize: 16, minHeight: 56, textAlignVertical: "top", fontFamily: Fonts.rounded }}
            />
          </Card>

          <Button
            title={machine ? `Start on ${machine.name}` : "Start"}
            kind="primary"
            icon="faceid"
            onPress={() => void go()}
            busy={busy}
            disabled={!ready}
            accessibilityHint="Asks for Face ID, fingerprint or passcode first"
          />
          {repo && chosenAgent && machine ? (
            <Body muted style={{ textAlign: "center", fontSize: 13 }}>
              Runs {chosenAgent.name || agentName(agents, chosenAgent.id)} on {machine.name} in {repo.name}. Its approvals will ask you.
            </Body>
          ) : null}
        </Animated.View>
      ) : null}
    </ScrollView>
  );
}
