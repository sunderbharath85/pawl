// Settings > Pets (advanced): the agents' pets. Only the default pet is ever on screen (it orchestrates every
// session); each agent still has a pet whose NAME labels its sessions (sessions are individuals), and here it can be
// renamed, given another species or a personality. A compact list: no crowd of dogs.
import { useEffect, useState } from "react";
import { SPECIES_LIST, SPECIES_META } from "@pawl/pet-engine";
import type { Pet, Species } from "@pawl/protocol";
import { checkPetName, groupByMachine, machineDisplayName, petRenameErrorText, readOnlyReason } from "@pawl/hub-client";
import { agentName, errorText, timeAgo } from "../../../lib/format";
import { usePanel } from "../../context";
import { SettingsSection } from "./shared";

export function PetsSection() {
  const { state, now } = usePanel();
  const pets = Object.values(state.pets).sort((a, b) => a.name.localeCompare(b.name));
  const groups = groupByMachine(state, pets);
  return (
    <SettingsSection
      id="pets"
      title="Pets (advanced)"
      description="Every agent has a pet whose name labels its sessions in the bubble's switcher and in this window. Only your default pet appears on screen."
    >
      {pets.length === 0 ? (
        <p className="hint">The hub adopts a pet for each agent it discovers. Connect to the hub to see them.</p>
      ) : (
        groups.map((g) => (
          <div key={g.machineId} className={`agent-pets${g.online ? "" : " is-offline"}`}>
            {groups.length > 1 && (
              <h3 className="agent-pets-machine">
                {g.local ? `${machineDisplayName(g.machine)} (this machine)` : `On ${machineDisplayName(g.machine)}`}
                {!g.online && <span className="muted"> · offline, last seen {timeAgo(g.machine?.lastSeen ?? 0, now)}</span>}
              </h3>
            )}
            {g.items.map((p) => (
              <AgentPetRow key={p.id} pet={p} />
            ))}
          </div>
        ))
      )}
    </SettingsSection>
  );
}

function AgentPetRow({ pet }: { pet: Pet }) {
  const { client, state, toast } = usePanel();
  const [name, setName] = useState(pet.name);
  const [species, setSpecies] = useState<Species>(pet.species);
  const [personality, setPersonality] = useState(pet.personality);
  const [saving, setSaving] = useState(false);
  /** Hub rejection of the last save (name_taken / invalid), shown under the name field. */
  const [nameError, setNameError] = useState<string | null>(null);
  // Another machine's pet is shown read-only (its own hub owns it).
  const readOnly = readOnlyReason(state, pet.machineId);
  // Names are unique across every known pet (all machines); checked as the user types.
  const nameCheck = name.trim() === pet.name ? null : checkPetName(name, pet.id, Object.values(state.pets));
  const liveNameError = nameCheck && !nameCheck.ok ? nameCheck.error : null;
  const shownNameError = nameError ?? liveNameError;
  const nameErrorId = `pet-name-error-${pet.id}`;
  const dirty = name !== pet.name || species !== pet.species || personality !== pet.personality;

  // Follow changes made elsewhere (another client renamed the pet) when not saving.
  useEffect(() => {
    if (!saving) {
      setName(pet.name);
      setSpecies(pet.species);
      setPersonality(pet.personality);
    }
  }, [pet.name, pet.species, pet.personality]);

  const save = async () => {
    if (!dirty || saving || readOnly) return;
    if (nameCheck && !nameCheck.ok) {
      setNameError(nameCheck.error);
      return;
    }
    setSaving(true);
    setNameError(null);
    const patch: Partial<Pick<Pet, "species" | "name" | "personality">> = {};
    if (nameCheck?.ok) patch.name = nameCheck.name;
    if (species !== pet.species) patch.species = species;
    if (personality !== pet.personality) patch.personality = personality;
    try {
      const updated = await client.updatePet(pet.id, patch);
      toast(`${updated.name} says ${SPECIES_META[updated.species].sound}`);
    } catch (err) {
      const friendly = patch.name !== undefined ? petRenameErrorText(err, patch.name) : undefined;
      if (friendly) setNameError(friendly);
      else toast(errorText(err), "error");
    } finally {
      setSaving(false);
    }
  };

  return (
    <form
      className={`agent-pet${readOnly ? " is-remote" : ""}`}
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <span className="agent-pet-agent">
        {agentName(state.agents, pet.agentId, pet.machineId)}
        {readOnly ? ` on ${machineDisplayName(state.machines[pet.machineId])}` : ""}
      </span>
      <span className="agent-pet-fields">
        <input
          disabled={!!readOnly}
          title={readOnly ?? undefined}
          className={`input${shownNameError ? " is-invalid" : ""}`}
          value={name}
          onChange={(e) => {
            setName(e.target.value);
            setNameError(null);
          }}
          aria-label={`Name of ${agentName(state.agents, pet.agentId, pet.machineId)}'s pet`}
          aria-invalid={shownNameError ? true : undefined}
          aria-describedby={shownNameError ? nameErrorId : undefined}
        />
        <select className="input select" disabled={!!readOnly} value={species} aria-label="Species" onChange={(e) => setSpecies(e.target.value as Species)}>
          {SPECIES_LIST.map((s) => (
            <option key={s} value={s}>
              {SPECIES_META[s].emoji} {SPECIES_META[s].label}
            </option>
          ))}
        </select>
        <input
          className="input"
          disabled={!!readOnly}
          value={personality}
          maxLength={120}
          placeholder="Personality: eager, loyal, a bit goofy"
          aria-label="Personality"
          onChange={(e) => setPersonality(e.target.value)}
        />
        {!readOnly && (
          <button type="submit" className="pill pill-primary" disabled={!dirty || saving || !!liveNameError}>
            {saving ? "Saving…" : "Save"}
          </button>
        )}
      </span>
      {shownNameError && (
        <span className="field-error" id={nameErrorId} role="alert">
          {shownNameError}
        </span>
      )}
      {readOnly && <span className="hint">{readOnly}</span>}
    </form>
  );
}
