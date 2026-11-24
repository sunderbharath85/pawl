// Bottom tabs (native): Pets (home), Approvals (badge = pending count), Computers, Settings.
import { NativeTabs } from "expo-router/native-tabs";
import { usePhone } from "@/lib/phone/phone-provider";
import { useTheme } from "@/theme";

export default function TabsLayout() {
  const t = useTheme();
  const { snapshot } = usePhone();
  const pending = Object.keys(snapshot.pending).length;
  return (
    <NativeTabs tintColor={t.accent} minimizeBehavior="onScrollDown">
      <NativeTabs.Trigger name="(home)">
        <NativeTabs.Trigger.Icon sf={{ default: "pawprint", selected: "pawprint.fill" }} md="pets" />
        <NativeTabs.Trigger.Label>Pets</NativeTabs.Trigger.Label>
      </NativeTabs.Trigger>
      <NativeTabs.Trigger name="approvals">
        <NativeTabs.Trigger.Icon sf={{ default: "checkmark.shield", selected: "checkmark.shield.fill" }} md="verified_user" />
        <NativeTabs.Trigger.Label>Approvals</NativeTabs.Trigger.Label>
        {pending > 0 ? <NativeTabs.Trigger.Badge>{pending > 9 ? "9+" : String(pending)}</NativeTabs.Trigger.Badge> : null}
      </NativeTabs.Trigger>
      <NativeTabs.Trigger name="machines">
        <NativeTabs.Trigger.Icon sf={{ default: "desktopcomputer", selected: "desktopcomputer" }} md="computer" />
        <NativeTabs.Trigger.Label>Computers</NativeTabs.Trigger.Label>
      </NativeTabs.Trigger>
      <NativeTabs.Trigger name="settings">
        <NativeTabs.Trigger.Icon sf={{ default: "gearshape", selected: "gearshape.fill" }} md="settings" />
        <NativeTabs.Trigger.Label>Settings</NativeTabs.Trigger.Label>
      </NativeTabs.Trigger>
    </NativeTabs>
  );
}
