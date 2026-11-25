// Settings > Voice (v0.2): talk to the pet. Off by default. Everything runs on this PC: the models are downloaded
// once from the k2-fsa project on GitHub (never bundled with Pawl), audio stays in memory and nothing is recorded.
import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { usePanel } from "../../context";
import { useSettings } from "../../../lib/settings";
import { speak, speechAvailable, stopSpeaking, useSystemVoices } from "../../../lib/speech";
import { useDefaultPetName } from "../../../lib/usePetName";
import {
  ASR_SIZE_MB,
  downloadFraction,
  formatBytes,
  hotkeyFromKey,
  hotkeyLabel,
  levelPercent,
  micMarker,
  MODEL_SIZE_MB,
  useVoice,
  type AsrModel,
  type VoiceModels,
  type VoiceState,
  type VoiceView,
} from "../../../lib/voice";
import { endWakeTest, startWakeTest } from "../../../lib/voiceShared";
import { SettingsSection, Toggle } from "./shared";

const WAKE_TEST_MS = 15_000;

function stateLine(state: VoiceState, name: string, hotkey: string | null): string {
  switch (state) {
    case "off":
      return "Off.";
    case "missing_models":
      return "The voice models are not downloaded yet.";
    case "downloading":
      return "Downloading the voice models…";
    case "loading":
      return "Loading the voice models…";
    case "idle":
      return `Ready: hold ${name}${hotkey ? ` or press ${hotkeyLabel(hotkey)}` : ""} to talk.`;
    case "wake_listening":
      return `Listening for “${name}”. The microphone is on.`;
    case "hearing":
      return "Listening to you…";
    case "transcribing":
      return "Turning what you said into text…";
    case "muted":
      return "Microphone muted. Unmute it from the pet's right-click menu or here.";
    case "error":
      return "Voice hit a problem.";
  }
}

export function VoiceSection() {
  const { client, state, meta, toast } = usePanel();
  const voice = useVoice();
  const { name } = useDefaultPetName(client, state, meta.phase === "connected");
  const status = voice.status;
  const config = status?.config;
  const [intro, setIntro] = useState(false);
  /** Turn voice on / switch to the accurate model once the download that was started for it has finished. */
  const [after, setAfter] = useState<{ models: VoiceModels } | null>(null);

  useEffect(() => {
    if (!after || !status) return;
    if (!status.models[after.models].installed) return;
    setAfter(null);
    setIntro(false);
    const patch = after.models === "default" ? { enabled: true } : { asrModel: "accurate" as const };
    voice.configure(patch).catch((err: unknown) => toast(String(err), "error"));
  }, [after, status, voice, toast]);

  if (!voice.available) {
    return (
      <SettingsSection id="voice" title="Voice" description="Talk to your pet: say its name, or hold it, and speak.">
        <p className="hint">Voice works in the Pawl desktop app (it is not available in a browser tab).</p>
      </SettingsSection>
    );
  }

  const download = (models: VoiceModels) => {
    setAfter({ models });
    voice.api?.download(models).catch((err: unknown) => {
      setAfter(null);
      toast(`Download failed: ${String(err)}`, "error");
    });
  };
  const cancelDownload = () => {
    setAfter(null);
    voice.api?.cancelDownload().catch((err: unknown) => toast(String(err), "error"));
  };
  const set = (patch: Parameters<VoiceView["configure"]>[0]) => voice.configure(patch).catch((err: unknown) => toast(String(err), "error"));

  const enabled = !!config?.enabled;
  const installed = !!status?.models.default.installed;
  const downloading = voice.state === "downloading" || after !== null;

  return (
    <SettingsSection
      id="voice"
      title="Voice"
      description={`Talk to ${name}: say its name, or hold it, and speak. Off until you turn it on. It runs on this PC: no audio leaves it, and nothing is recorded.`}
      aside={<MicState voice={voice} />}
    >
      <Toggle
        label={`Talk to ${name}`}
        hint={config ? stateLine(voice.state, name, config.hotkey) : "Asking the voice engine…"}
        checked={enabled || intro}
        disabled={!config}
        onChange={() => {
          if (enabled) {
            setIntro(false);
            void set({ enabled: false });
          } else if (installed) void set({ enabled: true });
          else setIntro((v) => !v);
        }}
      />
      {voice.error && voice.state === "error" && (
        <p className="field-error" role="alert">
          {voice.error}
        </p>
      )}

      {!enabled && (intro || downloading) && (
        <div className="voice-intro">
          <strong>Before voice turns on</strong>
          <ul>
            <li>
              Pawl downloads its voice models once: about {MODEL_SIZE_MB.default} MB from the k2-fsa project (sherpa-onnx) on GitHub. They are not part of
              Pawl's installer, and each file is checked against a known fingerprint.
            </li>
            <li>They run on this PC only. No audio leaves it, and nothing is recorded: while listening, sound is kept in memory for about 2 seconds.</li>
            <li>Turn voice off any time; “Delete voice models” below frees the space.</li>
          </ul>
          <DownloadRow voice={voice} models="default" downloading={downloading} onStart={() => download("default")} onCancel={cancelDownload} startLabel="Download and turn on" />
        </div>
      )}

      {enabled && config && status && (
        <>
          <MicPicker voice={voice} />
          <WakeWord voice={voice} name={name} />
          <label className="field">
            <span className="field-label">
              Sensitivity <span className="muted">{Math.round(config.sensitivity * 100)}%</span>
            </span>
            <SensitivitySlider value={config.sensitivity} onCommit={(v) => void set({ sensitivity: v })} />
            <span className="hint">Lower: fewer false wakes. Higher: it hears its name more easily, from farther away.</span>
          </label>
          <SpeechModel voice={voice} downloading={after?.models === "accurate"} onDownload={() => download("accurate")} onCancel={cancelDownload} onPick={(m) => void set({ asrModel: m })} />
          <HotkeyField value={config.hotkey} onChange={(h) => void set({ hotkey: h })} />
          <ReadAloud voice={voice} name={name} onToggle={() => void set({ readReplies: !config.readReplies })} />
        </>
      )}

      <DeleteModels voice={voice} />
    </SettingsSection>
  );
}

/** The section's corner: whether the microphone is on right now. */
function MicState({ voice }: { voice: VoiceView }) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(t);
  }, []);
  const marker = micMarker(voice, now);
  if (!marker) return null;
  return <span className={`voice-mic-state is-${marker}`}>{marker === "live" ? "● Microphone on" : "Microphone muted"}</span>;
}

function DownloadRow(props: { voice: VoiceView; models: VoiceModels; downloading: boolean; startLabel: string; onStart(): void; onCancel(): void }) {
  const d = props.voice.download?.models === props.models ? props.voice.download : null;
  const fraction = downloadFraction(d);
  if (!props.downloading) {
    return (
      <div className="form-actions">
        <button type="button" className="pill pill-primary" onClick={props.onStart}>
          {props.startLabel}
        </button>
      </div>
    );
  }
  return (
    <div className="voice-download" role="status">
      <div className="voice-progress" aria-hidden>
        <span style={{ width: `${Math.round((fraction ?? 0) * 100)}%` }} />
      </div>
      <span className="hint">
        {d && d.total > 0 ? `Downloading… ${formatBytes(d.received)} of ${formatBytes(d.total)}` : "Starting the download…"}
      </span>
      <button type="button" className="pill" onClick={props.onCancel}>
        Cancel
      </button>
    </div>
  );
}

/** Microphone choice and a live level meter (voice_mic_test while it is shown and on). */
function MicPicker({ voice }: { voice: VoiceView }) {
  const status = voice.status!;
  const [testing, setTesting] = useState(false);
  const api = voice.api;
  useEffect(() => {
    if (!testing || !api) return;
    api.micTest(true).catch(() => setTesting(false));
    return () => {
      api.micTest(false).catch(() => {});
    };
  }, [testing, api]);
  return (
    <div className="field">
      <label className="field-label" htmlFor="voice-mic">
        Microphone
      </label>
      <span className="voice-mic-row">
        <select
          id="voice-mic"
          className="input select"
          value={status.config.micId ?? ""}
          onChange={(e) => void voice.configure({ micId: e.target.value || null })}
        >
          <option value="">System default{status.mics.find((m) => m.isDefault) ? ` (${status.mics.find((m) => m.isDefault)!.name})` : ""}</option>
          {status.mics.map((m) => (
            <option key={m.id} value={m.id}>
              {m.name}
            </option>
          ))}
        </select>
        <button type="button" className={`pill${testing ? " pill-primary" : ""}`} aria-pressed={testing} onClick={() => setTesting((t) => !t)}>
          {testing ? "Stop test" : "Test microphone"}
        </button>
      </span>
      {testing && (
        <span className="voice-meter-row">
          <span className="voice-meter" role="meter" aria-label="Microphone level" aria-valuemin={0} aria-valuemax={100} aria-valuenow={levelPercent(voice.level)}>
            <span style={{ width: `${levelPercent(voice.level)}%` }} />
          </span>
          <span className="hint">Speak: the bar should move. The microphone is on while this test runs.</span>
        </span>
      )}
    </div>
  );
}

/** "Wake word: say <name>", the "say it like" spelling and a test that waits to hear it. */
function WakeWord({ voice, name }: { voice: VoiceView; name: string }) {
  const config = voice.status!.config;
  const [spoken, setSpoken] = useState(config.spokenAs ?? "");
  // The spotter gives no confidence (its score is always 1), so the test only says whether it heard the name.
  const [test, setTest] = useState<{ phase: "waiting" | "heard" | "missed" } | null>(null);
  useEffect(() => setSpoken(config.spokenAs ?? ""), [config.spokenAs]);

  // The test: wait for a wake event; the pet window leaves it alone while the test runs.
  const started = useRef(0);
  useEffect(() => {
    if (test?.phase !== "waiting" || !voice.wake || voice.wake.at < started.current) return;
    endWakeTest();
    voice.api?.listen("cancel").catch(() => {});
    setTest({ phase: "heard" });
  }, [voice.wake, test?.phase, voice.api]);
  useEffect(() => {
    if (test?.phase !== "waiting") return;
    const t = setTimeout(() => {
      endWakeTest();
      setTest({ phase: "missed" });
    }, WAKE_TEST_MS);
    return () => clearTimeout(t);
  }, [test?.phase]);
  useEffect(() => () => endWakeTest(), []);

  const saveSpoken = () => {
    const v = spoken.trim();
    if (v !== (config.spokenAs ?? "")) void voice.configure({ spokenAs: v || null });
  };
  const canTest = config.wakeWord && voice.state === "wake_listening";
  const said = (config.spokenAs ?? "").trim() || name;

  return (
    <>
      <Toggle
        label={`Wake word: say “${name}”`}
        hint="Only the pet's name wakes it; rename the pet and the wake word follows. Off: hold the pet or use the shortcut to talk."
        checked={config.wakeWord}
        onChange={() => void voice.configure({ wakeWord: !config.wakeWord })}
      />
      {config.wakeWord && (
        <div className="field">
          <label className="field-label" htmlFor="voice-spoken">
            Say it like <span className="muted">(optional)</span>
          </label>
          <span className="voice-mic-row">
            <input
              id="voice-spoken"
              className="input"
              value={spoken}
              maxLength={60}
              placeholder={name}
              onChange={(e) => setSpoken(e.target.value)}
              onBlur={saveSpoken}
              onKeyDown={(e) => {
                if (e.key === "Enter") saveSpoken();
              }}
            />
            <button
              type="button"
              className="pill"
              disabled={!canTest || test?.phase === "waiting"}
              title={canTest ? undefined : "Turn on the wake word and unmute the microphone to test it."}
              onClick={() => {
                saveSpoken();
                started.current = Date.now();
                startWakeTest(WAKE_TEST_MS);
                setTest({ phase: "waiting" });
              }}
            >
              Test
            </button>
          </span>
          <span className="hint" role="status">
            {test?.phase === "waiting"
              ? `Say “${said}” now…`
              : test?.phase === "heard"
                ? "Heard it!"
                : test?.phase === "missed"
                  ? "Didn't hear it. Try a spelling closer to how you say it (“Mo chee” for Mochi), or raise the sensitivity."
                  : "For unusual names: how the name sounds, in plain letters."}
          </span>
        </div>
      )}
    </>
  );
}

/** A slider that saves when the user lets go (not on every step). */
function SensitivitySlider({ value, onCommit }: { value: number; onCommit(v: number): void }) {
  const [v, setV] = useState(value);
  useEffect(() => setV(value), [value]);
  const commit = () => {
    if (v !== value) onCommit(v);
  };
  return (
    <input
      type="range"
      min={0}
      max={1}
      step={0.05}
      value={v}
      aria-label="Wake word sensitivity"
      onChange={(e) => setV(Number(e.target.value))}
      onPointerUp={commit}
      onKeyUp={commit}
      onBlur={commit}
    />
  );
}

function SpeechModel(props: { voice: VoiceView; downloading: boolean; onDownload(): void; onCancel(): void; onPick(m: AsrModel): void }) {
  const status = props.voice.status!;
  const current = status.config.asrModel;
  const options: Array<{ value: AsrModel; label: string; hint: string }> = [
    { value: "fast", label: `Fast (default, about ${ASR_SIZE_MB.fast} MB)`, hint: "Quick and light; part of the first download. Moonshine tiny (MIT licence)." },
    {
      value: "accurate",
      label: `Accurate (about ${ASR_SIZE_MB.accurate} MB)`,
      hint: status.models.accurate.installed ? "Better with names, code words and accents; a little slower." : "Better with names, code words and accents; a little slower. Downloads when you pick it.",
    },
  ];
  return (
    <fieldset className="field choice-group">
      <legend className="field-label">Speech model</legend>
      {options.map((o) => (
        <label key={o.value} className="choice">
          <input
            type="radio"
            name="voice-asr"
            value={o.value}
            checked={current === o.value}
            disabled={props.downloading}
            onChange={() => {
              if (o.value === "accurate" && !status.models.accurate.installed) props.onDownload();
              else props.onPick(o.value);
            }}
          />
          <span className="choice-text">
            <strong>{o.label}</strong>
            <span className="hint">{o.hint}</span>
          </span>
        </label>
      ))}
      {props.downloading && <DownloadRow voice={props.voice} models="accurate" downloading startLabel="" onStart={() => {}} onCancel={props.onCancel} />}
      <span className="hint voice-credit">
        Accurate uses Parakeet TDT 110M by NVIDIA, licensed CC-BY-4.0, in the k2-fsa project's sherpa-onnx format.
      </span>
    </fieldset>
  );
}

/** Push-to-talk shortcut: record a key combination (none by default). Rust registers it globally. */
function HotkeyField({ value, onChange }: { value: string | null; onChange(h: string | null): void }) {
  const [recording, setRecording] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (!recording) return;
    e.preventDefault();
    if (e.key === "Escape" && !e.ctrlKey && !e.altKey && !e.metaKey && !e.shiftKey) {
      setRecording(false);
      setError(null);
      return;
    }
    const res = hotkeyFromKey(e);
    if (!res) return;
    if (!res.ok) {
      setError(res.error);
      return;
    }
    setRecording(false);
    setError(null);
    onChange(res.hotkey);
  };
  return (
    <div className="field">
      <span className="field-label">Push-to-talk shortcut</span>
      <span className="voice-mic-row">
        <button
          type="button"
          className={`pill voice-hotkey${recording ? " is-recording" : ""}`}
          aria-describedby="voice-hotkey-note"
          onClick={() => {
            setRecording((r) => !r);
            setError(null);
          }}
          onKeyDown={onKeyDown}
          onBlur={() => setRecording(false)}
        >
          {recording ? "Press the keys…" : value ? hotkeyLabel(value) : "None: click to record"}
        </button>
        {value && !recording && (
          <button type="button" className="pill pill-ghost" onClick={() => onChange(null)}>
            Clear
          </button>
        )}
      </span>
      {error ? (
        <span className="field-error" id="voice-hotkey-note" role="alert">
          {error}
        </span>
      ) : (
        <span className="hint" id="voice-hotkey-note">
          Hold it to talk, let go to send, anywhere on this PC. Esc while recording cancels.
        </span>
      )}
    </div>
  );
}

function ReadAloud({ voice, name, onToggle }: { voice: VoiceView; name: string; onToggle(): void }) {
  const config = voice.status!.config;
  const [settings, setSettings] = useSettings();
  const voices = useSystemVoices();
  const available = speechAvailable();
  useEffect(() => () => stopSpeaking(), []);
  return (
    <>
      <Toggle
        label="Read replies aloud"
        hint={available ? `${name} says its replies with one of this PC's voices. It stops when you start talking or close the bubble.` : "This system has no speech voices."}
        checked={config.readReplies}
        disabled={!available}
        onChange={onToggle}
      />
      {config.readReplies && available && (
        <div className="voice-read">
          <label className="field">
            <span className="field-label">Voice</span>
            <select className="input select" value={settings.speechVoice} onChange={(e) => setSettings({ speechVoice: e.target.value })}>
              <option value="">System default</option>
              {voices.map((v) => (
                <option key={v.voiceURI} value={v.voiceURI}>
                  {v.name} ({v.lang})
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span className="field-label">
              Speed <span className="muted">{settings.speechRate.toFixed(1)}×</span>
            </span>
            <input type="range" min={0.5} max={2} step={0.1} value={settings.speechRate} onChange={(e) => setSettings({ speechRate: Number(e.target.value) })} />
          </label>
          <div className="form-actions">
            <button type="button" className="pill" onClick={() => speak(`Hi, I'm ${name}. This is how I sound.`, { voiceURI: settings.speechVoice, rate: settings.speechRate })}>
              Test voice
            </button>
          </div>
        </div>
      )}
    </>
  );
}

function DeleteModels({ voice }: { voice: VoiceView }) {
  const { toast } = usePanel();
  const [confirm, setConfirm] = useState(false);
  const models = voice.status?.models;
  if (!models) return null;
  const total = models.default.bytes + models.accurate.bytes;
  const parts = [
    models.default.installed ? `Wake word, voice detection and Fast: ${formatBytes(models.default.bytes)}` : null,
    models.accurate.installed ? `Accurate: ${formatBytes(models.accurate.bytes)}` : null,
  ].filter(Boolean);
  return (
    <div className="field voice-delete">
      <span className="field-label">Voice models on this PC</span>
      <span className="hint">{parts.length > 0 ? `${parts.join("; ")}.` : "None downloaded."}</span>
      {parts.length > 0 &&
        (confirm ? (
          <span className="voice-mic-row">
            <span>Delete {formatBytes(total)}? Voice stays off until you download them again.</span>
            <button
              type="button"
              className="pill pill-bad"
              onClick={() => {
                setConfirm(false);
                voice.api
                  ?.deleteModels("all")
                  .then(() => voice.refresh())
                  .catch((err: unknown) => toast(String(err), "error"));
              }}
            >
              Delete
            </button>
            <button type="button" className="pill pill-ghost" onClick={() => setConfirm(false)}>
              Keep
            </button>
          </span>
        ) : (
          <div className="form-actions">
            <button type="button" className="pill" onClick={() => setConfirm(true)}>
              Delete voice models
            </button>
          </div>
        ))}
    </div>
  );
}
