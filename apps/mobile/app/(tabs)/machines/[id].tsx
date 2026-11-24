// One computer: its sessions (open first, then recently ended), its pets, and a shortcut to start an agent there.
import { Stack, useLocalSearchParams, useRouter } from "expo-router";
import { ScrollView, Text, View } from "react-native";
import { PetSprite } from "@/components/pet/pet-sprite";
import { SessionRow } from "@/components/session-row";
import { Banner, Body, Button, Card, Divider, SectionTitle } from "@/components/ui";
import { isActiveSession, timeAgo } from "@/lib/format";
import { usePhone } from "@/lib/phone/phone-provider";
import { useTheme } from "@/theme";

export default function MachineScreen() {
  const t = useTheme();
  const router = useRouter();
  const { id } = useLocalSearchParams<{ id: string }>();
  const { snapshot } = usePhone();
  const hub = snapshot.hubs.find((h) => h.id === id);
  const agents = snapshot.agents[id] ?? [];
  const sessions = Object.values(snapshot.sessions)
    .filter((s) => s.machineId === id)
    .sort((a, b) => b.lastEventAt - a.lastEventAt);
  const open = sessions.filter(isActiveSession);
  const ended = sessions.filter((s) => !isActiveSession(s)).slice(0, 20);
  const pets = Object.values(snapshot.pets).filter((p) => p.machineId === id);

  if (!hub) {
    return (
      <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={{ padding: 16 }}>
        <Stack.Screen options={{ title: "Computer" }} />
        <Banner tone="warning" text="This computer is no longer paired with this phone." />
      </ScrollView>
    );
  }

  return (
    <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={{ padding: 16, gap: 14, paddingBottom: 48 }}>
      <Stack.Screen options={{ title: hub.name }} />
      {!hub.online ? (
        <Banner tone="info" text={`${hub.name} is offline${hub.lastSeen ? ` (seen ${timeAgo(hub.lastSeen)} ago)` : ""}. You're looking at what it last sent.`} />
      ) : null}
      {hub.truncated ? <Body muted style={{ fontSize: 13 }}>Older events were skipped while syncing; open the session on {hub.name} for the full history.</Body> : null}
      {pets.length > 0 ? (
        <View style={{ flexDirection: "row", gap: 12, flexWrap: "wrap" }}>
          {pets.map((p) => (
            <View key={p.id} style={{ alignItems: "center", width: 88 }}>
              <PetSprite species={p.species} activity={hub.online ? "idle" : "napping"} size={72} still />
              <Text style={{ color: t.text, fontWeight: "700" }} numberOfLines={1}>
                {p.name}
              </Text>
            </View>
          ))}
        </View>
      ) : null}
      <SectionTitle>Open sessions</SectionTitle>
      <Card>
        {open.length === 0 ? <Body muted>Nothing running right now.</Body> : null}
        {open.map((s, i) => (
          <View key={s.id}>
            {i > 0 ? <Divider /> : null}
            <SessionRow session={s} agents={agents} />
          </View>
        ))}
      </Card>
      <Button
        title={`Start a session on ${hub.name}`}
        kind="primary"
        icon="plus.circle.fill"
        disabled={!hub.online}
        onPress={() => router.push({ pathname: "/start", params: { machine: hub.id } })}
      />
      {ended.length > 0 ? (
        <>
          <SectionTitle>Recently finished</SectionTitle>
          <Card>
            {ended.map((s, i) => (
              <View key={s.id}>
                {i > 0 ? <Divider /> : null}
                <SessionRow session={s} agents={agents} dimmed />
              </View>
            ))}
          </Card>
        </>
      ) : null}
    </ScrollView>
  );
}
