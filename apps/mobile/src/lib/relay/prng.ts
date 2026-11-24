// Install a CSPRNG for tweetnacl on React Native. tweetnacl only auto-detects browser `crypto` or Node's
// `crypto` module; Hermes has neither, so key/nonce generation would throw "no PRNG".
import { getRandomValues } from "expo-crypto";
import nacl from "tweetnacl";

let installed = false;

export function installNaclPrng(): void {
  if (installed) return;
  nacl.setPRNG((x: Uint8Array, n: number) => {
    // expo-crypto caps a single call at 65536 bytes (like WebCrypto); chunk to be safe.
    const QUOTA = 65536;
    for (let i = 0; i < n; i += QUOTA) {
      const len = Math.min(QUOTA, n - i);
      const chunk = getRandomValues(new Uint8Array(len));
      x.set(chunk, i);
      chunk.fill(0);
    }
  });
  installed = true;
}
