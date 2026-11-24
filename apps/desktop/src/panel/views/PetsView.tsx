import { useEffect, useRef, useState } from "react";
import { SPECIES_META } from "@pawl/pet-engine";
import type { Pet, Species } from "@pawl/protocol";
import { agentName, errorText, timeAgo } from "../../lib/format";
import { checkPetName, groupByMachine, machineDisplayName, petRenameErrorText, readOnlyReason } from "@pawl/hub-client";
import { usePanel } from "../context";
import { ACTIVITY_LABEL, PetAvatar } from "../PetAvatar";

const SPECIES: Species[] = ["dog", "cat", "bird", "robot"];

export function PetsView({ focusPetId }: { focusPetId?: string }) {
  const { state, now } = usePanel();
  const pets = Object.values(state.pets).sort((a, b) => a.name.localeCompare(b.name));
  const groups = groupByMachine(state, pets);

  return (
    <section className="view">
      <header className="view-head">
        <h1>Pets</h1>
        <p>Every agent gets a dog by default. Rename them, or swap any of them for a cat, bird or robot.</p>
      </header>
      {pets.length === 0 ? (
        <div className="empty">
          <PetAvatar species="dog" activity="napping" size={110} />
          <h3>No pets yet</h3>
          <p>The hub adopts a pet for each agent it discovers. Connect to the hub to meet them.</p>
        </div>
      ) : (
        groups.map((g) => (
          <div key={g.machineId} className={`pet-group${g.online ? "" : " is-offline"}`}>
            {groups.length > 1 && (
              <h2 className="section-title">
                {g.local ? `${machineDisplayName(g.machine)} (this machine)` : `Visiting from ${machineDisplayName(g.machine)}`}
                {!g.online && <span className="muted"> · offline, last seen {timeAgo(g.machine?.lastSeen ?? 0, now)}</span>}
              </h2>
            )}
            <div className="pet-grid">
              {g.items.map((p) => (
                <PetCard key={p.id} pet={p} focused={p.id === focusPetId} />
              ))}
            </div>
          </div>
        ))
      )}
    </section>
  );
}

function PetCard({ pet, focused }: { pet: Pet; focused: boolean }) {
  const { client, state, views, toast } = usePanel();
  const [name, setName] = useState(pet.name);
  const [species, setSpecies] = useState<Species>(pet.species);
  const [personality, setPersonality] = useState(pet.personality);
  const [saving, setSaving] = useState(false);
  /** Hub rejection of the last save (name_taken / invalid), shown under the name field. */
  const [nameError, setNameError] = useState<string | null>(null);
  const ref = useRef<HTMLElement>(null);
  const view = views.find((v) => v.petId === pet.id);
  // Another machine's pet is shown read-only (its own hub owns it).
  const readOnly = readOnlyReason(state, pet.machineId);
  // Names are unique across every known pet (all machines); check as the user types.
  const nameCheck = name.trim() === pet.name ? null : checkPetName(name, pet.id, Object.values(state.pets));
  const liveNameError = nameCheck && !nameCheck.ok ? nameCheck.error : null;
  const shownNameError = nameError ?? liveNameError;
  const nameErrorId = `pet-name-error-${pet.id}`;

  // Follow server-side changes (another client renamed the pet) when not editing.
  const dirty = name !== pet.name || species !== pet.species || personality !== pet.personality;
  useEffect(() => {
    if (!saving) {
      setName(pet.name);
      setSpecies(pet.species);
      setPersonality(pet.personality);
    }
  }, [pet.name, pet.species, pet.personality]);

  useEffect(() => {
    if (focused) ref.current?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [focused]);

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
    <article ref={ref} className={`pet-card${focused ? " is-highlight" : ""}${readOnly ? " is-remote" : ""}`}>
      <div className="pet-card-stage">
        <PetAvatar species={species} activity={species !== pet.species ? "happy" : (view?.activity ?? "idle")} size={120} />
      </div>
      <div className="pet-card-body">
        <div className="pet-card-agent">
          Watches {agentName(state.agents, pet.agentId, pet.machineId)}
          {readOnly ? ` on ${machineDisplayName(state.machines[pet.machineId])}` : ""}
        </div>
        <input
          disabled={!!readOnly}
          title={readOnly ?? undefined}
          className={`input pet-name-input${shownNameError ? " is-invalid" : ""}`}
          value={name}
          onChange={(e) => {
            setName(e.target.value);
            setNameError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.nativeEvent.isComposing) void save();
          }}
          aria-label="Pet name"
          aria-invalid={shownNameError ? true : undefined}
          aria-describedby={shownNameError ? nameErrorId : undefined}
        />
        {shownNameError && (
          <span className="field-error" id={nameErrorId} role="alert">
            {shownNameError}
          </span>
        )}
        <div className="species-picker" role="radiogroup" aria-label="Species">
          {SPECIES.map((s) => (
            <button
              key={s}
              role="radio"
              aria-checked={species === s}
              className={`species-option${species === s ? " is-active" : ""}`}
              disabled={!!readOnly}
              onClick={() => setSpecies(s)}
              title={SPECIES_META[s].label}
            >
              <PetAvatar species={s} activity="idle" size={40} />
              <span>{SPECIES_META[s].label}</span>
            </button>
          ))}
        </div>
        <label className="field">
          <span className="field-label">Personality</span>
          <input
            className="input"
            disabled={!!readOnly}
            value={personality}
            maxLength={120}
            placeholder="eager, loyal, a bit goofy"
            onChange={(e) => setPersonality(e.target.value)}
          />
        </label>
        <div className="pet-stats">
          <div title={`Mood ${Math.round(pet.mood)}/100`}>
            <span className="muted">Mood</span>
            <div className="mood">
              <span style={{ width: `${Math.max(4, Math.min(100, pet.mood))}%` }} />
            </div>
          </div>
          <div>
            <span className="muted">XP</span> <strong>{pet.xp}</strong>
          </div>
          <div className="muted">{view ? ACTIVITY_LABEL[view.activity] : ""}</div>
        </div>
        {readOnly && <span className="remote-note">{readOnly}</span>}
        <div className="form-actions" hidden={!!readOnly}>
          <button className="pill pill-primary" disabled={!dirty || saving || !!liveNameError} onClick={() => void save()}>
            {saving ? "Saving…" : "Save"}
          </button>
          {dirty && (
            <button
              className="pill pill-ghost"
              onClick={() => {
                setName(pet.name);
                setSpecies(pet.species);
                setPersonality(pet.personality);
                setNameError(null);
              }}
            >
              Reset
            </button>
          )}
        </div>
      </div>
    </article>
  );
}
