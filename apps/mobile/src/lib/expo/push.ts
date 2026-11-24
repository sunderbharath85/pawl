// Push wakes: the relay sends one content-free notification ("Your pet needs you") when a frame is queued for this
// phone while it is offline. The phone registers its Expo push token with the relay (`register_push`) after pairing.
import Constants from "expo-constants";
import * as Device from "expo-device";
import * as Notifications from "expo-notifications";

export type PushSetup =
  | { ok: true; token: string }
  | { ok: false; reason: "simulator" | "denied" | "no_project" | "error"; message: string };

/** The EAS project id Expo push tokens need (app.json `extra.eas.projectId`), if configured. */
export function easProjectId(): string | null {
  const extra = Constants.expoConfig?.extra as { eas?: { projectId?: unknown } } | undefined;
  const id = extra?.eas?.projectId ?? Constants.easConfig?.projectId;
  return typeof id === "string" && id.length > 0 ? id : null;
}

/** Foreground wakes are not shown as banners: the app already shows the approval itself. */
export function installNotificationHandler(): void {
  Notifications.setNotificationHandler({
    handleNotification: async () => ({
      shouldShowBanner: false,
      shouldShowList: true,
      shouldPlaySound: false,
      shouldSetBadge: false,
    }),
  });
}

/** Ask for permission (once) and get the Expo push token. */
export async function setupPush(): Promise<PushSetup> {
  if (!Device.isDevice) {
    return { ok: false, reason: "simulator", message: "Push needs a real phone (simulators and emulators cannot receive it)." };
  }
  const projectId = easProjectId();
  if (!projectId) {
    return {
      ok: false,
      reason: "no_project",
      message: "Push isn't set up in this build (no EAS project id). Approvals still work while the app is open.",
    };
  }
  try {
    if (process.env.EXPO_OS === "android") {
      await Notifications.setNotificationChannelAsync("default", {
        name: "Pet needs you",
        importance: Notifications.AndroidImportance.HIGH,
      });
    }
    let { status } = await Notifications.getPermissionsAsync();
    if (status !== "granted") status = (await Notifications.requestPermissionsAsync()).status;
    if (status !== "granted") {
      return { ok: false, reason: "denied", message: "Notifications are off for Pawl. Turn them on in the phone's settings." };
    }
    const token = await Notifications.getExpoPushTokenAsync({ projectId });
    return { ok: true, token: token.data };
  } catch {
    return { ok: false, reason: "error", message: "Couldn't get a push token right now. Pawl will try again." };
  }
}

/**
 * Push off: the relay keeps the last registered token (there is no unregister frame), so unregister this device
 * from push here; the relay's wake then fails at Expo and nothing reaches the phone.
 */
export async function disablePush(): Promise<void> {
  try {
    await Notifications.unregisterForNotificationsAsync();
  } catch {
    // Not registered: nothing to do.
  }
}
