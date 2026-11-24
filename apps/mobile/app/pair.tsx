// Pair: scan the QR code the desktop shows under "Pair a phone" (or paste the link, or open a pawl:// link), check
// what it is, then confirm. Nothing pairs without the confirm tap. The invite's code, relay token and access key are
// never shown or logged.
import { CameraView, useCameraPermissions } from "expo-camera";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useEffect, useRef, useState } from "react";
import { ScrollView, Text, TextInput, View } from "react-native";
import Animated, { FadeIn, FadeInDown } from "react-native-reanimated";
import { PetSprite } from "@/components/pet/pet-sprite";
import { Banner, Body, Button, Card, Title, tapHaptic } from "@/components/ui";
import { extractInviteUri, inviteSummary, PairError, tryParseInvite, type HubView, type InviteSummary } from "@/lib/peer";
import { usePhone } from "@/lib/phone/phone-provider";
import { Fonts, Radius, useTheme } from "@/theme";

type Step =
  | { kind: "scan" }
  | { kind: "confirm"; uri: string; summary: InviteSummary }
  | { kind: "pairing"; summary: InviteSummary }
  | { kind: "done"; hub: HubView };

const PAIR_ERROR_TEXT: Record<string, string> = {
  bad_uri: "That isn't a Pawl pairing code.",
  no_relay: "That's a computer-to-computer invite. On the desktop, use Machines & pairing → Pair a phone to get a phone QR code.",
  other_relay: "That computer uses a different relay than the ones already paired. Pair phones within one Pawl group.",
  self_invite: "That code belongs to this phone.",
  busy: "Already pairing — one moment.",
  unreachable: "Couldn't reach the relay. Check this phone's internet connection, then scan again.",
  timeout: "The computer didn't answer in time. Make sure Pawl is running there, then try a fresh code.",
  invite_rejected: "That code was already used or has expired. Make a new one on the computer.",
};

function pairErrorText(e: unknown): string {
  if (e instanceof PairError) {
    if (e.code === "unreachable" && e.detail === "bad_auth") return "The relay couldn't verify this phone. Try again in a moment.";
    return PAIR_ERROR_TEXT[e.code] ?? `Pairing failed (${e.code}).`;
  }
  return "Pairing failed. Try a fresh code.";
}

function Scanner({ onCode }: { onCode: (text: string) => void }) {
  const t = useTheme();
  const [permission, request] = useCameraPermissions();
  const locked = useRef(false);
  if (!permission) return <View style={{ height: 300 }} />;
  if (!permission.granted) {
    return (
      <Card style={{ alignItems: "center" }}>
        <Body style={{ textAlign: "center" }}>Pawl needs the camera to scan the pairing QR code on your computer's screen.</Body>
        {permission.canAskAgain ? (
          <Button title="Allow camera" kind="primary" onPress={() => void request()} style={{ alignSelf: "stretch" }} />
        ) : (
          <Body muted style={{ textAlign: "center" }}>Camera access is off for Pawl. Turn it on in Settings, or paste the link below.</Body>
        )}
      </Card>
    );
  }
  return (
    <View style={{ height: 320, borderRadius: Radius.xl, borderCurve: "continuous", overflow: "hidden", borderWidth: 3, borderColor: t.accent }}>
      <CameraView
        style={{ flex: 1 }}
        facing="back"
        barcodeScannerSettings={{ barcodeTypes: ["qr"] }}
        onBarcodeScanned={(r) => {
          if (locked.current) return;
          locked.current = true;
          onCode(r.data);
          setTimeout(() => {
            locked.current = false;
          }, 1500);
        }}
      />
    </View>
  );
}

export default function PairScreen() {
  const t = useTheme();
  const router = useRouter();
  const { node, snapshot } = usePhone();
  const params = useLocalSearchParams<{ uri?: string }>();
  const [step, setStep] = useState<Step>({ kind: "scan" });
  const [paste, setPaste] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  const consider = (text: string) => {
    const uri = extractInviteUri(text) ?? text.trim();
    const inv = tryParseInvite(uri);
    if (!inv) {
      setError(PAIR_ERROR_TEXT.bad_uri!);
      tapHaptic("error");
      return;
    }
    if (!inv.relay) {
      setError(PAIR_ERROR_TEXT.no_relay!);
      tapHaptic("error");
      return;
    }
    setError(null);
    tapHaptic("success");
    setStep({ kind: "confirm", uri, summary: inviteSummary(inv) });
  };

  // Opened from a pawl://peer link: go straight to the confirm step.
  useEffect(() => {
    if (typeof params.uri === "string" && params.uri) consider(params.uri);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [params.uri]);

  useEffect(() => {
    if (step.kind !== "confirm") return;
    const h = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(h);
  }, [step.kind]);

  const pair = async (uri: string, summary: InviteSummary) => {
    setStep({ kind: "pairing", summary });
    setError(null);
    try {
      const hub = await node.pair(uri);
      tapHaptic("success");
      setStep({ kind: "done", hub });
    } catch (e) {
      tapHaptic("error");
      setError(pairErrorText(e));
      setStep({ kind: "scan" });
    }
  };

  return (
    <ScrollView contentInsetAdjustmentBehavior="automatic" keyboardShouldPersistTaps="handled" contentContainerStyle={{ padding: 16, gap: 16, paddingBottom: 48 }}>
      {error ? <Banner tone="danger" icon="exclamationmark.triangle.fill" text={error} /> : null}

      {step.kind === "scan" ? (
        <Animated.View entering={FadeIn} style={{ gap: 16 }}>
          <Body muted>
            On your computer, open Pawl → Machines & pairing → <Text style={{ fontWeight: "800", color: t.text }}>Pair a phone</Text>, then point this camera at the QR code.
          </Body>
          <Scanner onCode={consider} />
          <Card>
            <Text style={{ color: t.textMuted, fontWeight: "700" }}>Or paste the link</Text>
            <TextInput
              value={paste}
              onChangeText={(v) => {
                setPaste(v);
                // A whole link pasted at once: go on without another tap.
                if (v.length - paste.length > 40 && tryParseInvite(extractInviteUri(v) ?? v)) consider(v);
              }}
              placeholder="pawl://peer?…"
              placeholderTextColor={t.textFaint}
              autoCapitalize="none"
              autoCorrect={false}
              accessibilityLabel="Pairing link"
              style={{ color: t.text, fontSize: 15, fontFamily: Fonts.mono, borderWidth: 1, borderColor: t.border, borderRadius: Radius.sm, padding: 10, minHeight: 44 }}
            />
            <Button title="Continue" kind="primary" onPress={() => consider(paste)} disabled={!paste.trim()} />
          </Card>
        </Animated.View>
      ) : null}

      {step.kind === "confirm" ? (
        <Animated.View entering={FadeInDown} style={{ gap: 16 }}>
          <View style={{ alignItems: "center" }}>
            <PetSprite species="dog" activity="alert" size={140} />
          </View>
          <Card tone="accent">
            <Title>Pair with this computer?</Title>
            <Body muted>It will see this phone as “{snapshot.name}”. This phone will see that computer's sessions, pets and approvals, and can approve, deny, prompt and start agents there.</Body>
            <Detail label="Computer id" value={step.summary.machineId} />
            <Detail label="Key fingerprint" value={step.summary.fingerprint} />
            <Detail label="Relay" value={step.summary.relayHost ?? "—"} />
            <Detail
              label="Code expires"
              value={step.summary.expiresAt > now ? `in ${Math.max(1, Math.round((step.summary.expiresAt - now) / 60_000))} min` : "expired (by this phone's clock)"}
            />
          </Card>
          <Button title="Pair" kind="primary" icon="checkmark.circle.fill" onPress={() => void pair(step.uri, step.summary)} />
          <Button title="Scan a different code" kind="ghost" onPress={() => setStep({ kind: "scan" })} />
        </Animated.View>
      ) : null}

      {step.kind === "pairing" ? (
        <View style={{ alignItems: "center", gap: 12, paddingTop: 24 }}>
          <PetSprite species="dog" activity="working" size={160} walking />
          <Title>Fetching…</Title>
          <Body muted style={{ textAlign: "center" }}>Saying hello to {step.summary.machineId} through {step.summary.relayHost}.</Body>
        </View>
      ) : null}

      {step.kind === "done" ? (
        <Animated.View entering={FadeInDown} style={{ alignItems: "center", gap: 12, paddingTop: 16 }}>
          <PetSprite species="dog" activity="happy" size={180} />
          <Title style={{ fontSize: 26 }}>Paired with {step.hub.name}!</Title>
          <Body muted style={{ textAlign: "center" }}>
            Every other computer {step.hub.name} is paired with joins on its own. You'll get a nudge when a pet needs you.
          </Body>
          <Button title="Meet the pets" kind="primary" onPress={() => router.dismissTo("/")} style={{ alignSelf: "stretch" }} />
        </Animated.View>
      ) : null}
    </ScrollView>
  );
}

function Detail({ label, value }: { label: string; value: string }) {
  const t = useTheme();
  return (
    <View style={{ flexDirection: "row", justifyContent: "space-between", gap: 12 }}>
      <Text style={{ color: t.textMuted, fontSize: 14 }}>{label}</Text>
      <Text selectable style={{ color: t.text, fontSize: 14, fontWeight: "700", fontFamily: Fonts.mono, flexShrink: 1, textAlign: "right" }}>
        {value}
      </Text>
    </View>
  );
}
