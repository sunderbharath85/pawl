// Expo adapters for the platform-free peer core: secure storage, CSPRNG, biometrics, device facts.
import * as Crypto from "expo-crypto";
import * as Device from "expo-device";
import * as LocalAuthentication from "expo-local-authentication";
import * as SecureStore from "expo-secure-store";
import nacl from "tweetnacl";
import { stripNameChars, type AuthCapability, type Authenticator, type KeyValueStore, type PhoneOs, type RandomBytes } from "@/lib/peer";

/**
 * Keychain / Keystore. `AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY`: readable while the app runs in the background (seq
 * state), never synced to iCloud or restored to another device, so the X25519 secret never leaves this phone.
 */
const SECURE_OPTS: SecureStore.SecureStoreOptions = {
  keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY,
};

export const secureStore: KeyValueStore = {
  get: (key) => SecureStore.getItemAsync(key, SECURE_OPTS),
  set: (key, value) => SecureStore.setItemAsync(key, value, SECURE_OPTS),
  delete: (key) => SecureStore.deleteItemAsync(key, SECURE_OPTS),
};

/** Native CSPRNG (expo-crypto `getRandomValues`; the sync `getRandomBytes` may fall back to Math.random in dev). */
export const expoRandomBytes: RandomBytes = (n) => {
  const out = new Uint8Array(n);
  for (let i = 0; i < n; i += 1024) {
    const chunk = Crypto.getRandomValues(new Uint8Array(Math.min(1024, n - i)));
    out.set(chunk, i);
    chunk.fill(0);
  }
  return out;
};

let prngInstalled = false;

/**
 * tweetnacl only auto-detects browser or Node crypto; Hermes has neither. The core passes randomness explicitly,
 * but install the native PRNG too so any `nacl.randomBytes` call is safe.
 */
export function installNaclPrng(): void {
  if (prngInstalled) return;
  nacl.setPRNG((x, n) => x.set(expoRandomBytes(n)));
  prngInstalled = true;
}

export const phoneOs: PhoneOs = process.env.EXPO_OS === "ios" ? "ios" : process.env.EXPO_OS === "android" ? "android" : "other";

/** A friendly default device name ("Pixel 9", "iPhone"); the user can rename it in Settings. */
export function defaultDeviceName(): string {
  const raw = Device.deviceName || Device.modelName || (phoneOs === "ios" ? "iPhone" : "Phone");
  // The rule the hubs apply to peer names: no control or invisible format characters, 1..=40 characters.
  return [...stripNameChars(raw).trim()].slice(0, 40).join("").trim() || "Phone";
}

/** expo-local-authentication behind the pure gate's interface. */
export const expoAuthenticator: Authenticator = {
  async capability(): Promise<AuthCapability> {
    const [hasHardware, isEnrolled, level, types] = await Promise.all([
      LocalAuthentication.hasHardwareAsync(),
      LocalAuthentication.isEnrolledAsync(),
      LocalAuthentication.getEnrolledLevelAsync(),
      LocalAuthentication.supportedAuthenticationTypesAsync(),
    ]);
    const kinds: AuthCapability["kinds"] = [];
    if (types.includes(LocalAuthentication.AuthenticationType.FACIAL_RECOGNITION)) kinds.push("face");
    if (types.includes(LocalAuthentication.AuthenticationType.FINGERPRINT)) kinds.push("fingerprint");
    if (types.includes(LocalAuthentication.AuthenticationType.IRIS)) kinds.push("iris");
    const securityLevel = Math.max(0, Math.min(3, Number(level))) as AuthCapability["securityLevel"];
    return { hasHardware, isEnrolled, securityLevel, kinds };
  },
  async authenticate({ reason, allowPasscode }) {
    const res = await LocalAuthentication.authenticateAsync({
      promptMessage: reason,
      cancelLabel: "Cancel",
      // Biometrics first; the system offers the device passcode as the fallback (or alone when no biometrics).
      disableDeviceFallback: !allowPasscode,
      requireConfirmation: true,
      biometricsSecurityLevel: "weak",
    });
    return res.success ? { success: true } : { success: false, error: res.error };
  },
};
