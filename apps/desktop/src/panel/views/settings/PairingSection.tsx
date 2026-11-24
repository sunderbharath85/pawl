// Machines & pairing: the peer listener (on/off + address, applied at runtime by the hub), whether devices introduced
// by paired hubs are accepted (step 7), and a way to the Machines view. `ListenerControl` is also used by the first-run
// setup and the Machines view's invite card.
import { useEffect, useMemo, useState } from "react";
import { isWildcardListen, linkPathLabel, machineCounts, machinesLine, pairingAllowsText, phoneCounts, phonesLine } from "@krypto/hub-client";
import { PEER_DEFAULT_PORT, type HubSettings } from "@krypto/protocol";
import { isTauri, native } from "../../../lib/native";
import { introsForm, introsPatch, pairingForm, pairingPatch } from "../../../lib/settingsForms";
import { usePanel } from "../../context";
import { errorFor, Field, OverrideNote, SaveBar, SettingsSection, Toggle, useDraft, useSectionSave } from "./shared";
import type { SectionProps } from "./sections";

/** This machine's LAN address with the default peer port, if the OS reports one. */
function useSuggestedListen(): string | null {
  const [lan, setLan] = useState<string | null>(null);
  useEffect(() => {
    if (!isTauri) return;
    native
      .lanAddress()
      .then((ip) => setLan(ip ? `${ip.includes(":") ? `[${ip}]` : ip}:${PEER_DEFAULT_PORT}` : null))
      .catch(() => setLan(null));
  }, []);
  return lan;
}

/** Peer listener on/off and its address. Turning it on (or moving it) asks first, saying what it allows. */
export function ListenerControl({ settings, live, compact }: { settings: HubSettings; live: boolean; compact?: boolean }) {
  const { state } = usePanel();
  const source = useMemo(() => pairingForm(settings), [settings]);
  const { draft, update, dirty, reset } = useDraft(source);
  const save = useSectionSave(["peer.listen"]);
  const suggested = useSuggestedListen();
  const listening = state.peer?.listening ?? null;
  const pinned = !!settings.overriddenBy["peer.listen"];

  const turn = () => {
    if (draft.on) update({ on: false });
    else update({ on: true, listen: draft.listen || settings.peer.listen || suggested || "" });
  };

  // What the hub is doing right now vs. what is saved (a bind can fail, e.g. the address is taken).
  let liveStatus: string;
  if (listening) liveStatus = `Listening on ${listening}.`;
  else if (settings.peer.listen) liveStatus = `Saved as ${settings.peer.listen}, but the hub is not listening (the address may be in use or not on this machine).`;
  else liveStatus = "Off. This machine can still join other machines' invites; it cannot create invites.";

  return (
    <div className={`listener-control${compact ? " is-compact" : ""}`}>
      <Toggle
        label="Peer listener"
        hint={live ? liveStatus : "Connect the hub to change this."}
        checked={draft.on}
        disabled={!live || save.saving || pinned}
        onChange={turn}
      />
      <OverrideNote settings={settings} field="peer.listen" />
      {draft.on && (
        <Field
          label="Listen on"
          error={errorFor(save.errors, "peer.listen")}
          hint={
            isWildcardListen(draft.listen)
              ? "0.0.0.0 / :: listens on every network this computer is on; invites then need this machine's LAN address."
              : `This machine's IP address on your network, with a port (${PEER_DEFAULT_PORT} by default).`
          }
        >
          {(a) => (
            <span className="input-row">
              <input
                id={a.id}
                className={`input mono${a.invalid ? " is-invalid" : ""}`}
                value={draft.listen}
                placeholder={suggested ?? `192.168.1.20:${PEER_DEFAULT_PORT}`}
                spellCheck={false}
                disabled={!live || save.saving || pinned}
                aria-invalid={a.invalid || undefined}
                aria-describedby={a.describedBy}
                onChange={(e) => update({ listen: e.target.value })}
              />
              {suggested && suggested !== draft.listen && (
                <button className="pill" disabled={!live || save.saving} onClick={() => update({ listen: suggested })}>
                  Use {suggested}
                </button>
              )}
            </span>
          )}
        </Field>
      )}
      <SaveBar
        dirty={dirty}
        saving={save.saving}
        message={save.message}
        disabled={!live}
        saveLabel={draft.on && !settings.peer.listen ? "Turn on" : !draft.on && settings.peer.listen ? "Turn off" : "Save"}
        onReset={() => (reset(), save.clear())}
        onSave={() => void save.run(pairingPatch(draft, settings), { success: draft.on ? "Peer listener saved" : "Peer listener off" })}
      />
    </div>
  );
}

/**
 * "Accept devices introduced by paired hubs" (`peer.acceptRosterIntros`). Turning it on asks first (a paired hub can
 * then add devices here); off is applied without asking. Hidden text for a hub that predates it.
 */
function RosterIntrosControl({ settings, live }: Pick<SectionProps, "settings" | "live">) {
  const source = useMemo(() => introsForm(settings) ?? { accept: false }, [settings]);
  const supported = introsForm(settings) !== null;
  const { draft, update, dirty, reset } = useDraft(source);
  const save = useSectionSave(["peer.acceptRosterIntros"]);
  if (!supported) {
    return <p className="hint">Update krypto on this machine to choose whether paired machines may introduce new devices here.</p>;
  }
  const pinned = !!settings.overriddenBy["peer.acceptRosterIntros"];
  return (
    <div className="intros-control">
      <Toggle
        label="Accept devices introduced by paired hubs"
        hint={
          draft.accept
            ? "On: when one of your paired machines pairs a new phone or machine, it introduces it here and this machine adds it too, so you pair once. An introduction never replaces a device already paired here, and each one shows a notification."
            : "Off: introductions are noted (you get a notification) but nothing is added; pair each phone or machine here yourself."
        }
        checked={draft.accept}
        disabled={!live || save.saving || pinned}
        onChange={() => update({ accept: !draft.accept })}
      />
      <OverrideNote settings={settings} field="peer.acceptRosterIntros" />
      <SaveBar
        dirty={dirty}
        saving={save.saving}
        message={save.message}
        disabled={!live}
        onReset={() => (reset(), save.clear())}
        onSave={() =>
          void save.run(introsPatch(draft, settings), { success: draft.accept ? "Introduced devices are accepted" : "Introduced devices are not added" })
        }
      />
    </div>
  );
}

export function PairingSection({ settings, live }: SectionProps) {
  const { state, navigate } = usePanel();
  const counts = machineCounts(state);
  const phones = phoneCounts(state);
  const linked = state.devices.filter((d) => d.path);
  return (
    <SettingsSection
      id="pairing"
      title="Machines & pairing"
      description="Pair the Krypto hubs on your machines so each one sees the others' sessions and pets. Only the machine that creates an invite needs its listener on."
      aside={
        <button className="pill" onClick={() => navigate({ view: "machines" })}>
          Open Machines →
        </button>
      }
    >
      <p className="pairing-allows">{pairingAllowsText(settings.peer.acceptIntents)}</p>
      <ListenerControl settings={settings} live={live} />
      <RosterIntrosControl settings={settings} live={live} />
      <p className="hint">
        {counts.total > 1 ? `${machinesLine(counts.total, live ? counts.online : undefined)}.` : "No machines paired yet."}{" "}
        {phones.total > 0 ? `${phonesLine(phones.total, live ? phones.online : undefined)}. ` : ""}
        {linked.length > 0 &&
          `Linked now: ${linked.map((d) => `${d.name} (${linkPathLabel(d.path)})`).join(", ")}. `}
        Create invites, pair a phone, join a machine and unpair in Machines.
      </p>
    </SettingsSection>
  );
}
