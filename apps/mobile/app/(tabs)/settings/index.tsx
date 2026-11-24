// Settings: this phone (name), relay status, paired computers (remove one), push on/off, recent roster notices,
// approvals lock status, and "Unpair this phone" (bye to every computer, new identity).
import { relayStatusText } from "@pawl/hub-client";
import { useRouter } from "expo-router";
import { useEffect, useState } from "react";
import { Alert, ScrollView, Switch, Text, TextInput, View } from "react-native";
import { Banner, Body, Button, Card, Divider, Dot, Row, SectionTitle } from "@/components/ui";
import { keyFingerprint, decode, PHONE_NAME_MAX } from "@/lib/peer";
import { timeAgo } from "@/lib/format";
import { easProjectId } from "@/lib/expo/push";
import { useGateAvailability } from "@/lib/phone/actions";
import { usePhone } from "@/lib/phone/phone-provider";
import { Fonts, Radius, useTheme } from "@/theme";

export default function SettingsScreen() {
  const t = useTheme();
  const router = useRouter();
  const { node, snapshot, push, setPushEnabled } = usePhone();
  const { availability, refresh } = useGateAvailability();
  const [name, setName] = useState(snapshot.name);
  const [nameError, setNameError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const relay = relayStatusText(snapshot.relay);
  const pushAvailable = easProjectId() !== null;

  useEffect(() => setName(snapshot.name), [snapshot.name]);

  const saveName = () => {
    try {
      node.setName(name);
      setNameError(null);
    } catch {
      setNameError(`1 to ${PHONE_NAME_MAX} characters, no control characters.`);
    }
  };

  const removeHub = (id: string, label: string) => {
    Alert.alert(`Remove ${label}?`, `This phone stops seeing ${label} and tells it so. Other computers stay paired. You can pair it again with a new code.`, [
      { text: "Cancel", style: "cancel" },
      { text: "Remove", style: "destructive", onPress: () => void node.removeHub(id) },
    ]);
  };

  const unpair = () => {
    Alert.alert(
      "Unpair this phone?",
      "Every computer is told to forget this phone, and this phone forgets its key and all pairings. To use Pawl here again, pair with a new code.",
      [
        { text: "Cancel", style: "cancel" },
        {
          text: "Unpair",
          style: "destructive",
          onPress: () => {
            setBusy(true);
            void node.unpairPhone().finally(() => setBusy(false));
          },
        },
      ],
    );
  };

  return (
    <ScrollView contentInsetAdjustmentBehavior="automatic" keyboardShouldPersistTaps="handled" contentContainerStyle={{ padding: 16, gap: 12, paddingBottom: 48 }}>
      <SectionTitle>This phone</SectionTitle>
      <Card>
        <Text style={{ color: t.textMuted, fontSize: 13 }}>Name shown on your computers</Text>
        <TextInput
          value={name}
          onChangeText={setName}
          onSubmitEditing={saveName}
          onBlur={saveName}
          maxLength={PHONE_NAME_MAX}
          returnKeyType="done"
          accessibilityLabel="Phone name"
          style={{ color: t.text, fontSize: 17, fontWeight: "600", borderWidth: 1, borderColor: t.border, borderRadius: Radius.sm, padding: 10 }}
        />
        {nameError ? <Banner tone="danger" text={nameError} /> : null}
        <Row title="Device id" subtitle={snapshot.deviceId} />
        <Row title="Key fingerprint" subtitle={keyFingerprint(decode(snapshot.publicKey))} />
      </Card>

      <SectionTitle>Relay</SectionTitle>
      <Card>
        <Row title={relay.tone === "connected" ? "Connected" : relay.tone === "off" ? "Off" : relay.tone === "error" ? "Not connected" : "Connecting…"} subtitle={relay.text} right={<Dot on={relay.tone === "connected"} />} />
        <Body muted style={{ fontSize: 13 }}>
          The relay only passes along encrypted messages; it never sees what your agents do. The phone always connects through it.
        </Body>
      </Card>

      <SectionTitle right={<Button title="Pair" kind="ghost" onPress={() => router.push("/pair")} />}>Paired computers</SectionTitle>
      <Card>
        {snapshot.hubs.length === 0 ? <Body muted>None yet.</Body> : null}
        {snapshot.hubs.map((h, i) => (
          <View key={h.id}>
            {i > 0 ? <Divider /> : null}
            <Row
              title={h.name}
              subtitle={`${h.online ? "Online" : h.lastSeen ? `Seen ${timeAgo(h.lastSeen)} ago` : "Not seen yet"} · paired ${timeAgo(h.pairedAt)} ago${h.introducedBy ? ` · added by ${snapshot.machines[h.introducedBy]?.name ?? "another computer"}` : ""}`}
              left={<Dot on={h.online} />}
              right={<Button title="Remove" kind="ghost" onPress={() => removeHub(h.id, h.name)} />}
            />
          </View>
        ))}
      </Card>

      <SectionTitle>Notifications</SectionTitle>
      <Card>
        <Row
          title="Wake me when a pet needs me"
          subtitle="A content-free push (“Your pet needs you”) when an approval waits while the app is closed."
          right={
            <Switch
              value={pushAvailable && snapshot.pushEnabled}
              disabled={!pushAvailable}
              onValueChange={(v) => void setPushEnabled(v)}
              trackColor={{ true: t.accent }}
              accessibilityLabel="Push notifications"
            />
          }
        />
        {!pushAvailable ? (
          <Body muted style={{ fontSize: 13 }}>
            Push needs an EAS project id in this build's app config (extra.eas.projectId). Approvals still work while Pawl is open.
          </Body>
        ) : null}
        {pushAvailable && snapshot.pushEnabled && push && !push.ok ? <Banner tone="warning" text={push.message} /> : null}
        {pushAvailable && snapshot.pushEnabled && snapshot.pushToken ? <Body muted style={{ fontSize: 13 }}>Push is set up for this phone.</Body> : null}
      </Card>

      <SectionTitle>Approvals lock</SectionTitle>
      <Card>
        {availability ? (
          availability.available ? (
            <Body>Approving and starting agents ask for {availability.label} first.</Body>
          ) : (
            <Banner tone="warning" icon="lock.shield.fill" text={availability.explanation} />
          )
        ) : (
          <Body muted>Checking…</Body>
        )}
        <Button title="Check again" kind="ghost" onPress={refresh} />
      </Card>

      {snapshot.notices.length > 0 ? (
        <>
          <SectionTitle>Recent</SectionTitle>
          <Card>
            {snapshot.notices.slice(0, 10).map((n) => (
              <Text key={n.id} style={{ color: n.level === "warning" ? t.warning : t.text, fontSize: 14 }}>
                {n.text} <Text style={{ color: t.textFaint, fontSize: 12 }}>· {timeAgo(n.ts)}</Text>
              </Text>
            ))}
          </Card>
        </>
      ) : null}

      {snapshot.paired ? (
        <Button title="Unpair this phone" kind="danger" icon="trash.fill" onPress={unpair} busy={busy} style={{ marginTop: 8 }} />
      ) : null}
      <Text style={{ color: t.textFaint, fontSize: 12, textAlign: "center", fontFamily: Fonts.rounded }}>Pawl · open source (MIT)</Text>
    </ScrollView>
  );
}
