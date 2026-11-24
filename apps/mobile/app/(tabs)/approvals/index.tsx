// Approvals: every machine's pending permission requests. Approve = Face ID / fingerprint / passcode first, then an
// intent with confirmed: true to the owning computer; deny needs no check. Decisions made elsewhere show who made
// them ("Approved from Windows PC").
import { describePermission } from "@krypto/pet-engine";
import type { PermissionRequest } from "@krypto/protocol";
import { useRouter } from "expo-router";
import { useState } from "react";
import { ScrollView, Text, View } from "react-native";
import Animated, { FadeIn, FadeOutUp, LinearTransition } from "react-native-reanimated";
import { PetSprite } from "@/components/pet/pet-sprite";
import { Banner, Body, Button, Card, Icon, Pill, SectionTitle, Title } from "@/components/ui";
import { approveOption } from "@/lib/peer";
import { decisionText, machineName, useActions, useGateAvailability } from "@/lib/phone/actions";
import { usePhone } from "@/lib/phone/phone-provider";
import { agentName, shortPath, timeAgo } from "@/lib/format";
import { useTheme } from "@/theme";

const KIND_LABEL: Record<string, string> = {
  execute: "Run a command",
  edit: "Edit files",
  delete: "Delete",
  move: "Move files",
  read: "Read",
  search: "Search",
  fetch: "Use the network",
  think: "Think",
  switch_mode: "Switch mode",
  other: "Tool",
};

function ApprovalCard({ request, gateReady }: { request: PermissionRequest; gateReady: boolean }) {
  const t = useTheme();
  const router = useRouter();
  const { snapshot } = usePhone();
  const actions = useActions();
  const [busy, setBusy] = useState<"approve" | "deny" | null>(null);
  const [sent, setSent] = useState<"approve" | "deny" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const machine = machineName(snapshot, request.machineId);
  const online = snapshot.machines[request.machineId]?.online === true;
  const pet = Object.values(snapshot.pets).find((p) => p.machineId === request.machineId && p.agentId === request.agentId);
  const agents = snapshot.agents[request.machineId] ?? [];
  const canApprove = approveOption(request) !== null;
  const kind = request.toolCall.kind ?? "other";
  const risky = kind === "execute" || kind === "delete" || kind === "fetch" || kind === "other";
  const path = request.toolCall.locations?.[0]?.path;

  const run = async (which: "approve" | "deny") => {
    setBusy(which);
    setError(null);
    const res = which === "approve" ? await actions.approve(request) : await actions.deny(request);
    setBusy(null);
    if (res.ok) setSent(which);
    else if (!res.cancelled) setError(res.message);
  };

  return (
    <Animated.View entering={FadeIn} exiting={FadeOutUp} layout={LinearTransition}>
      <Card tone="alert">
        <View style={{ flexDirection: "row", gap: 12, alignItems: "center" }}>
          <PetSprite species={pet?.species ?? "dog"} activity="alert" size={64} />
          <View style={{ flex: 1, gap: 2 }}>
            <Text style={{ color: t.text, fontSize: 17, fontWeight: "800" }}>{pet?.name ?? agentName(agents, request.agentId)} needs you</Text>
            <Text style={{ color: t.textMuted, fontSize: 13 }}>
              {agentName(agents, request.agentId)} on <Text style={{ fontWeight: "800", color: t.text }}>{machine}</Text> · {timeAgo(request.createdAt)}
            </Text>
          </View>
        </View>
        <Text selectable style={{ color: t.text, fontSize: 17, fontWeight: "600", lineHeight: 23 }}>
          {describePermission(request)}
        </Text>
        <View style={{ flexDirection: "row", gap: 8, flexWrap: "wrap" }}>
          <Pill text={KIND_LABEL[kind] ?? "Tool"} fg={risky ? t.danger : t.info} bg={risky ? t.dangerSoft : t.infoSoft} />
          {path ? <Pill text={shortPath(path)} fg={t.textMuted} bg={t.backgroundAlt} /> : null}
        </View>
        {!online ? <Banner tone="warning" text={`${machine} is offline right now. You can answer once it reconnects, or on the computer itself.`} /> : null}
        {error ? <Banner tone="danger" text={error} icon="exclamationmark.triangle.fill" /> : null}
        {sent ? (
          <Banner tone="success" icon="checkmark.circle.fill" text={`${sent === "approve" ? "Approved" : "Denied"} — ${machine} is on it.`} />
        ) : (
          <View style={{ flexDirection: "row", gap: 10 }}>
            <Button title="Deny" kind="danger" onPress={() => void run("deny")} busy={busy === "deny"} disabled={busy !== null || !online} style={{ flex: 1 }} />
            <Button
              title="Approve"
              kind="primary"
              icon="faceid"
              onPress={() => void run("approve")}
              busy={busy === "approve"}
              disabled={busy !== null || !online || !canApprove || !gateReady}
              style={{ flex: 1.4 }}
              accessibilityHint="Asks for Face ID, fingerprint or passcode first"
            />
          </View>
        )}
        {!canApprove ? <Body muted>This request only offers “always allow”. Choose that on {machine} itself.</Body> : null}
        <Button title="See the session" kind="ghost" onPress={() => router.push({ pathname: "/session/[id]", params: { id: request.sessionId } })} />
      </Card>
    </Animated.View>
  );
}

export default function ApprovalsScreen() {
  const t = useTheme();
  const { snapshot, node } = usePhone();
  const { availability } = useGateAvailability();
  const pending = Object.values(snapshot.pending).sort((a, b) => a.createdAt - b.createdAt);
  const recent = snapshot.resolutions.slice(0, 12);
  const gateReady = availability?.available === true;

  return (
    <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={{ padding: 16, gap: 14, paddingBottom: 48 }}>
      {availability && !availability.available ? <Banner tone="warning" icon="lock.shield.fill" text={`Approving is off on this phone. ${availability.explanation}`} /> : null}
      {availability?.available ? (
        <Text style={{ color: t.textMuted, fontSize: 13 }}>
          Approvals ask for {availability.label} first. Denying never does.
        </Text>
      ) : null}
      {pending.length === 0 ? (
        <Card style={{ alignItems: "center", paddingVertical: 26 }}>
          <PetSprite species="dog" activity="happy" size={140} />
          <Title>All quiet</Title>
          <Body muted style={{ textAlign: "center" }}>
            {snapshot.paired ? "No agent is waiting on you. Go stretch your legs." : "Pair a computer first — its approvals will show up here."}
          </Body>
        </Card>
      ) : (
        pending.map((r) => <ApprovalCard key={r.requestId} request={r} gateReady={gateReady} />)
      )}
      {recent.length > 0 ? (
        <>
          <SectionTitle>Recently decided</SectionTitle>
          <Card>
            {recent.map((r) => (
              <View key={r.requestId} style={{ flexDirection: "row", gap: 10, alignItems: "center", paddingVertical: 4 }}>
                <Icon
                  name={r.verdict === "approve" ? "checkmark.circle.fill" : "xmark.circle.fill"}
                  color={r.verdict === "approve" ? t.success : t.textMuted}
                  size={18}
                />
                <View style={{ flex: 1 }}>
                  <Text style={{ color: t.text, fontSize: 15 }} numberOfLines={1}>
                    {r.request ? describePermission(r.request) : "A request"}
                  </Text>
                  <Text style={{ color: t.textMuted, fontSize: 12 }}>
                    {decisionText(snapshot, r, (id) => node.deviceName(id))} · {timeAgo(r.at)}
                  </Text>
                </View>
              </View>
            ))}
          </Card>
        </>
      ) : null}
    </ScrollView>
  );
}
