// Deep links: `pawl://peer?...` (the phone invite a hub shows as a QR code / link) opens the Pair screen with the
// full URI, which then asks the user to confirm. Nothing pairs without that tap.
export function redirectSystemPath({ path }: { path: string; initial: boolean }): string {
  try {
    if (/^pawl:\/\/peer\?/i.test(path)) return `/pair?uri=${encodeURIComponent(path)}`;
  } catch {
    // fall through
  }
  return path;
}
