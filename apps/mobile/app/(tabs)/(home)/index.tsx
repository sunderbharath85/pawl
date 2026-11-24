// Pets: the dog is the hero. Not paired yet -> a friendly welcome that leads to pairing. Paired -> the pet that needs
// you most up top (with its bubble), then every pet on every computer.
import { petLine, type PetView } from "@krypto/pet-engine";
import { useRouter } from "expo-router";
import { Pressable, ScrollView, Text, View } from "react-native";
import Animated, { FadeInDown, LinearTransition } from "react-native-reanimated";
import { PetSprite } from "@/components/pet/pet-sprite";
import { Body, Button, Card, Pill, SectionTitle, Title } from "@/components/ui";
import { usePhone } from "@/lib/phone/phone-provider";
import { usePetViews } from "@/lib/phone/pets";
import { activityColor, ACTIVITY_LABEL, Fonts, Radius, useTheme } from "@/theme";

function Welcome() {
  const t = useTheme();
  const router = useRouter();
  const steps = [
    "On your computer, open Krypto → Machines & pairing → Pair a phone.",
    "Scan the QR code with this phone (or paste the link).",
    "Your pets show up here. Approve from anywhere with Face ID or your fingerprint.",
  ];
  return (
    <Animated.View entering={FadeInDown.duration(400)} style={{ gap: 18 }}>
      <View style={{ alignItems: "center", gap: 8, paddingTop: 12 }}>
        <View style={{ backgroundColor: t.accentSoft, borderRadius: 999, padding: 18 }}>
          <PetSprite species="dog" activity="idle" size={170} />
        </View>
        <Text style={{ color: t.text, fontSize: 28, fontWeight: "800", fontFamily: Fonts.rounded, textAlign: "center" }}>Woof! Let's get paired.</Text>
        <Body muted style={{ textAlign: "center", maxWidth: 340 }}>
          Krypto's dogs watch your coding agents on every computer. Pair this phone once and they'll come find you when an agent needs a yes or no.
        </Body>
      </View>
      <Card>
        {steps.map((s, i) => (
          <View key={s} style={{ flexDirection: "row", gap: 12, alignItems: "flex-start" }}>
            <View style={{ width: 26, height: 26, borderRadius: 13, backgroundColor: t.accent, alignItems: "center", justifyContent: "center" }}>
              <Text style={{ color: t.accentText, fontWeight: "800", fontVariant: ["tabular-nums"] }}>{i + 1}</Text>
            </View>
            <Body style={{ flex: 1 }}>{s}</Body>
          </View>
        ))}
      </Card>
      <Button title="Pair with a computer" kind="primary" icon="qrcode.viewfinder" onPress={() => router.push("/pair")} />
    </Animated.View>
  );
}

function Hero({ view }: { view: PetView }) {
  const t = useTheme();
  const router = useRouter();
  const c = activityColor(t, view.activity);
  const permission = view.bubble?.kind === "permission";
  const line = view.bubble?.text ?? petLine(view.species, view.activity, view.petId.length + view.since);
  return (
    <Animated.View entering={FadeInDown.duration(350)} layout={LinearTransition}>
      <Card tone={permission ? "alert" : undefined} style={{ alignItems: "center", paddingVertical: 22 }}>
        <View
          accessibilityLabel={`${view.name} says: ${line}`}
          style={{
            backgroundColor: permission ? t.dangerSoft : t.bubble,
            borderColor: permission ? t.danger : t.border,
            borderWidth: 1,
            borderRadius: Radius.lg,
            borderCurve: "continuous",
            paddingHorizontal: 16,
            paddingVertical: 10,
            maxWidth: "90%",
          }}
        >
          <Text style={{ color: permission ? t.danger : t.text, fontSize: 16, fontWeight: "600", textAlign: "center" }}>{line}</Text>
        </View>
        <PetSprite species={view.species} activity={view.activity} size={190} still={view.remoteOffline} />
        <View style={{ alignItems: "center", gap: 6 }}>
          <Text style={{ color: t.text, fontSize: 26, fontWeight: "800", fontFamily: Fonts.rounded }}>{view.name}</Text>
          <View style={{ flexDirection: "row", gap: 8, alignItems: "center" }}>
            <Pill text={ACTIVITY_LABEL[view.activity]} fg={c.fg} bg={c.bg} />
            <Text style={{ color: t.textMuted, fontSize: 14 }}>on {view.machineName ?? "a computer"}</Text>
          </View>
        </View>
        {permission ? (
          <Button title="Review approval" kind="primary" icon="checkmark.circle.fill" onPress={() => router.push("/approvals")} style={{ alignSelf: "stretch" }} />
        ) : view.sessionId ? (
          <Button title="Open session" onPress={() => router.push({ pathname: "/session/[id]", params: { id: view.sessionId! } })} style={{ alignSelf: "stretch" }} />
        ) : null}
      </Card>
    </Animated.View>
  );
}

function PetTile({ view }: { view: PetView }) {
  const t = useTheme();
  const router = useRouter();
  const c = activityColor(t, view.activity);
  const open = () => {
    if (view.sessionId) router.push({ pathname: "/session/[id]", params: { id: view.sessionId } });
    else router.push({ pathname: "/machines/[id]", params: { id: view.machineId } });
  };
  return (
    <Pressable
      onPress={open}
      accessibilityRole="button"
      accessibilityLabel={`${view.name} on ${view.machineName ?? "a computer"}, ${ACTIVITY_LABEL[view.activity]}`}
      style={({ pressed }) => ({ flexBasis: "47%", flexGrow: 1, opacity: view.remoteOffline ? 0.5 : pressed ? 0.8 : 1 })}
    >
      <Card style={{ alignItems: "center", gap: 6, padding: 12 }}>
        <PetSprite species={view.species} activity={view.activity} size={92} still />
        <Text style={{ color: t.text, fontSize: 17, fontWeight: "800", fontFamily: Fonts.rounded }} numberOfLines={1}>
          {view.name}
        </Text>
        <Text style={{ color: t.textMuted, fontSize: 12 }} numberOfLines={1}>
          {view.machineName ?? "Computer"}
          {view.remoteOffline ? " · offline" : ""}
        </Text>
        <Pill text={ACTIVITY_LABEL[view.activity]} fg={c.fg} bg={c.bg} style={{ alignSelf: "center" }} />
      </Card>
    </Pressable>
  );
}

export default function PetsScreen() {
  const t = useTheme();
  const router = useRouter();
  const { snapshot } = usePhone();
  const views = usePetViews(snapshot);
  const online = snapshot.hubs.filter((h) => h.online).length;
  const waiting = Object.keys(snapshot.pending).length;
  const [lead, ...rest] = views;

  return (
    <ScrollView contentInsetAdjustmentBehavior="automatic" contentContainerStyle={{ padding: 16, gap: 16, paddingBottom: 48 }}>
      {!snapshot.paired ? (
        <Welcome />
      ) : (
        <>
          <Text style={{ color: t.textMuted, fontSize: 14, fontVariant: ["tabular-nums"] }}>
            {snapshot.hubs.length} {snapshot.hubs.length === 1 ? "computer" : "computers"} · {online} online
            {waiting > 0 ? ` · ${waiting} waiting for you` : ""}
          </Text>
          {lead ? (
            <Hero view={lead} />
          ) : (
            <Card style={{ alignItems: "center", paddingVertical: 24 }}>
              <PetSprite species="dog" activity="napping" size={150} />
              <Title>{online > 0 ? "Fetching your pets…" : "Your computers are asleep"}</Title>
              <Body muted style={{ textAlign: "center" }}>
                {online > 0
                  ? "They'll trot in as soon as the computers send them over."
                  : "Krypto reaches them over the relay as soon as one is back online."}
              </Body>
            </Card>
          )}
          {rest.length > 0 ? (
            <>
              <SectionTitle>The pack</SectionTitle>
              <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 12 }}>
                {rest.map((v) => (
                  <PetTile key={v.petId} view={v} />
                ))}
              </View>
            </>
          ) : null}
          <Button title="Start a session" icon="plus.circle.fill" onPress={() => router.push("/start")} disabled={online === 0} />
        </>
      )}
    </ScrollView>
  );
}
