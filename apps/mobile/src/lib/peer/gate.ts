// The approval gate: approve and start send `confirmed: true` only after the phone's owner proved presence with
// biometrics (Face ID, Touch ID, fingerprint, face unlock) or, without biometrics, the device passcode. Neither
// available -> approvals and starts are disabled with an explanation. Pure: the platform check is injected (the
// Expo adapter wraps expo-local-authentication), so the decisions are unit-tested with mocks.

/** What the device can do (expo-local-authentication's hasHardware / isEnrolled / getEnrolledLevel). */
export interface AuthCapability {
  /** Biometric hardware present. */
  hasHardware: boolean;
  /** Biometrics enrolled. */
  isEnrolled: boolean;
  /** 0 none, 1 passcode / PIN / pattern only, 2 weak biometrics, 3 strong biometrics (SecurityLevel). */
  securityLevel: 0 | 1 | 2 | 3;
  /** "face" / "fingerprint" / "iris" as supported. */
  kinds: ("face" | "fingerprint" | "iris")[];
}

export type AuthFailure =
  | "user_cancel"
  | "system_cancel"
  | "app_cancel"
  | "lockout"
  | "not_enrolled"
  | "passcode_not_set"
  | "not_available"
  | "authentication_failed"
  | "timeout"
  | "user_fallback"
  | "unknown"
  | (string & {});

export interface Authenticator {
  capability(): Promise<AuthCapability>;
  /** One prompt. `allowPasscode` = the system may fall back to (or use only) the device passcode. */
  authenticate(opts: { reason: string; allowPasscode: boolean }): Promise<{ success: true } | { success: false; error: AuthFailure }>;
}

export type GateMethod = "biometric" | "passcode";

export type GateAvailability =
  | { available: true; method: GateMethod; label: string }
  | { available: false; explanation: string };

export type GateResult =
  | { confirmed: true; method: GateMethod }
  | { confirmed: false; code: "unavailable" | "cancelled" | "failed" | "lockout" | "busy"; message: string };

export function biometricLabel(cap: Pick<AuthCapability, "kinds">, platform: "ios" | "android" | "other" = "other"): string {
  if (cap.kinds.includes("face")) return platform === "ios" ? "Face ID" : "face unlock";
  if (cap.kinds.includes("fingerprint")) return platform === "ios" ? "Touch ID" : "fingerprint";
  if (cap.kinds.includes("iris")) return "iris scan";
  return "biometrics";
}

/** Biometrics if enrolled; else the passcode if one is set; else unavailable (with what to do about it). */
export function gateAvailability(cap: AuthCapability, platform: "ios" | "android" | "other" = "other"): GateAvailability {
  if (cap.hasHardware && cap.isEnrolled && cap.securityLevel >= 2) {
    return { available: true, method: "biometric", label: biometricLabel(cap, platform) };
  }
  if (cap.securityLevel >= 1) {
    return { available: true, method: "passcode", label: platform === "ios" ? "your passcode" : "your screen lock" };
  }
  return {
    available: false,
    explanation:
      "Approving from this phone needs a screen lock. Set up Face ID, a fingerprint or a passcode in the phone's settings, then come back.",
  };
}

const CANCELLED = new Set(["user_cancel", "system_cancel", "app_cancel", "user_fallback"]);

export class ApprovalGate {
  readonly #auth: Authenticator;
  readonly #platform: "ios" | "android" | "other";
  #inFlight: Promise<GateResult> | null = null;

  constructor(auth: Authenticator, platform: "ios" | "android" | "other" = "other") {
    this.#auth = auth;
    this.#platform = platform;
  }

  async availability(): Promise<GateAvailability> {
    try {
      return gateAvailability(await this.#auth.capability(), this.#platform);
    } catch {
      return { available: false, explanation: "Krypto could not check this phone's screen lock. Try again." };
    }
  }

  /**
   * Ask the owner to confirm `reason` ("Approve on Mac mini: run bun test"). Only `{confirmed: true}` may lead to
   * an intent with `confirmed: true`, and only for the action that asked: a call while another prompt is open is
   * refused (`busy`) instead of sharing that prompt's result, so one success can never approve two things.
   */
  confirm(reason: string): Promise<GateResult> {
    if (this.#inFlight) {
      return Promise.resolve({ confirmed: false, code: "busy", message: "Finish the confirmation that is already open first." });
    }
    const run = this.#confirm(reason).finally(() => {
      this.#inFlight = null;
    });
    this.#inFlight = run;
    return run;
  }

  async #confirm(reason: string): Promise<GateResult> {
    const avail = await this.availability();
    if (!avail.available) return { confirmed: false, code: "unavailable", message: avail.explanation };
    let res: Awaited<ReturnType<Authenticator["authenticate"]>>;
    try {
      res = await this.#auth.authenticate({ reason, allowPasscode: true });
    } catch {
      return { confirmed: false, code: "failed", message: "The check did not work. Try again." };
    }
    // Strictly `true`: a malformed result never counts as a success.
    if (res.success === true) return { confirmed: true, method: avail.method };
    const error = res.error;
    if (CANCELLED.has(error)) return { confirmed: false, code: "cancelled", message: "Cancelled — nothing was sent." };
    if (error === "lockout") {
      return { confirmed: false, code: "lockout", message: "Too many tries. Unlock the phone with its passcode, then try again." };
    }
    if (error === "not_enrolled" || error === "passcode_not_set" || error === "not_available") {
      return { confirmed: false, code: "unavailable", message: gateAvailabilityText(this.#platform) };
    }
    return { confirmed: false, code: "failed", message: "That didn't match. Nothing was sent." };
  }
}

function gateAvailabilityText(platform: "ios" | "android" | "other"): string {
  return platform === "ios"
    ? "Set up Face ID, Touch ID or a passcode in Settings to approve from this phone."
    : "Set up a fingerprint, face unlock or a screen lock in Settings to approve from this phone.";
}
