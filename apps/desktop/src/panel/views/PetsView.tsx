import { useEffect, useRef, useState } from "react";
import { SPECIES_META } from "@ai-pet/pet-engine";
import type { Pet, Species } from "@ai-pet/protocol";
import { agentName, errorText } from "../../lib/format";
import { usePanel } from "../context";
import { ACTIVITY_LABEL, PetAvatar } from "../PetAvatar";

const SPECIES: Species[] = ["dog", "cat", "bird", "robot"];

export function PetsView({ focusPetId }: { focusPetId?: string }) {
  const { state } = usePanel();
  const pets = Object.values(state.pets).sort((a, b) => a.name.localeCompare(b.name));

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
        <div className="pet-grid">
          {pets.map((p) => (
            <PetCard key={p.id} pet={p} focused={p.id === focusPetId} />
          ))}
        </div>
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
  const ref = useRef<HTMLElement>(null);
  const view = views.find((v) => v.petId === pet.id);

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
    if (!dirty || !name.trim()) return;
    setSaving(true);
    try {
      const patch: Partial<Pick<Pet, "species" | "name" | "personality">> = {};
      if (name.trim() !== pet.name) patch.name = name.trim();
      if (species !== pet.species) patch.species = species;
      if (personality !== pet.personality) patch.personality = personality;
      await client.updatePet(pet.id, patch);
      toast(`${name.trim()} says ${SPECIES_META[species].sound}`);
    } catch (err) {
      toast(errorText(err), "error");
    } finally {
      setSaving(false);
    }
  };

  return (
    <article ref={ref} className={`pet-card${focused ? " is-highlight" : ""}`}>
      <div className="pet-card-stage">
        <PetAvatar species={species} activity={species !== pet.species ? "happy" : (view?.activity ?? "idle")} size={120} />
      </div>
      <div className="pet-card-body">
        <div className="pet-card-agent">Watches {agentName(state.agents, pet.agentId)}</div>
        <input
          className="input pet-name-input"
          value={name}
          maxLength={40}
          onChange={(e) => setName(e.target.value)}
          aria-label="Pet name"
        />
        <div className="species-picker" role="radiogroup" aria-label="Species">
          {SPECIES.map((s) => (
            <button
              key={s}
              role="radio"
              aria-checked={species === s}
              className={`species-option${species === s ? " is-active" : ""}`}
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
        <div className="form-actions">
          <button className="pill pill-primary" disabled={!dirty || saving || !name.trim()} onClick={() => void save()}>
            {saving ? "Saving…" : "Save"}
          </button>
          {dirty && (
            <button
              className="pill pill-ghost"
              onClick={() => {
                setName(pet.name);
                setSpecies(pet.species);
                setPersonality(pet.personality);
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
