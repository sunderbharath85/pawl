// Computers: every paired hub, online state and path (the phone always goes through the relay), what is running.
import { relayStatusText } from "@pawl/hub-client";
import { useRouter } from "expo-router";
import { ScrollView, Text, View } from "react-native";
import Animated, { FadeIn, LinearTransition } from "react-native-reanimated";
import { PetSprite } from "@/components/pet/pet-sprite";
import { Banner, Body, Button, Card, Dot, Icon, Row, Title } from "@/components/ui";
import { isActiveSession, timeAgo } from "@/lib/format";
import { usePhone } from "@/lib/phone/phone-provider";
import { useTheme } from "@/theme";

const OS_LABEL: Record<string, string> = { windows: "Windows", macos: "macOS", linux: "Linux", ios: "iOS", android: "Android", other: "Computer" };

export default function MachinesScreen() {
  const t = useTheme();
  const router = useRouter();
  const { snapshot } = usePhone();
  const relay = relayStatusText(snapshot.relay);
  const sessions = Object.values(snapshot.sessions);

  return (
    <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={{ padding: 16, gap: 14, paddingBottom: 48 }}>
      {snapshot.paired && relay.tone !== "connected" ? (
        <Banner tone={relay.tone === "error" ? "danger" : "info"} icon="antenna.radiowaves.left.and.right" text={relay.text} />
      ) : null}
      {snapshot.hubs.length === 0 ? (
        <Card style={{ alignItems: "center", paddingVertical: 26 }}>
          <PetSprite species="dog" activity="idle" size={130} />
          <Title>No computers yet</Title>
          <Body muted style={{ textAlign: "center" }}>
            Pair one and every computer it is paired with joins automatically.
          </Body>
          <Button title="Pair with a computer" kind="primary" icon="qrcode.viewfinder" onPress={() => router.push("/pair")} style={{ alignSelf: "stretch" }} />
        </Card>
      ) : (
        snapshot.hubs.map((h) => {
          const mine = sessions.filter((s) => s.machineId === h.id);
          const active = mine.filter(isActiveSession).length;
          const waiting = Object.values(snapshot.pending).filter((p) => p.machineId === h.id).length;
          const pets = Object.values(snapshot.pets).filter((p) => p.machineId === h.id);
          const status = h.online
            ? `Online · via relay${h.sync === "syncing" ? " · syncing" : ""}`
            : h.refusedSince
              ? "Not accepting this phone yet"
              : h.lastSeen
                ? `Offline · seen ${timeAgo(h.lastSeen)} ago`
                : "Waiting to connect";
          const parts = [`${active} active ${active === 1 ? "session" : "sessions"}`];
          if (waiting) parts.push(`${waiting} waiting`);
          if (pets.length) parts.push(pets.map((p) => p.name).join(", "));
          return (
            <Animated.View key={h.id} entering={FadeIn} layout={LinearTransition}>
              <Card>
                <Row
                  title={h.name}
                  subtitle={`${OS_LABEL[h.os ?? "other"] ?? "Computer"} · ${status}`}
                  left={<Icon name={h.os === "macos" ? "laptopcomputer" : "desktopcomputer"} color={h.online ? t.accent : t.textFaint} size={26} />}
                  right={<Dot on={h.online} />}
                  onPress={() => router.push({ pathname: "/machines/[id]", params: { id: h.id } })}
                  chevron
                />
                <Text style={{ color: t.textMuted, fontSize: 13 }}>{parts.join(" · ")}</Text>
                {h.introducedBy ? (
                  <Text style={{ color: t.textFaint, fontSize: 12 }}>
                    Added by {snapshot.machines[h.introducedBy]?.name ?? "another computer"}
                  </Text>
                ) : null}
                {h.refusedSince ? (
                  <Body muted style={{ fontSize: 13 }}>
                    The relay says {h.name} doesn't accept this phone. It may still be learning about it — or it unpaired this phone.
                  </Body>
                ) : null}
              </Card>
            </Animated.View>
          );
        })
      )}
      {snapshot.paired ? (
        <View style={{ gap: 10 }}>
          <Button title="Start a session" icon="plus.circle.fill" onPress={() => router.push("/start")} disabled={!snapshot.hubs.some((h) => h.online)} />
          <Button title="Pair another computer" kind="ghost" onPress={() => router.push("/pair")} />
        </View>
      ) : null}
    </ScrollView>
  );
}
