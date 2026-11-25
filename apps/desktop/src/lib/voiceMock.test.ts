import { describe, expect, test } from "bun:test";
import { INITIAL_VOICE, voiceBackend, voiceReduce, type VoiceEvent, type VoiceModel } from "./voice";
import { createVoiceMock } from "./voiceMock";

const until = async (fn: () => boolean, ms = 8000) => {
  const end = Date.now() + ms;
  while (!fn()) {
    if (Date.now() > end) throw new Error("timed out");
    await new Promise((r) => setTimeout(r, 20));
  }
};

describe("the contract flow, through the dev mock and the state mapping", () => {
  test("outside the Tauri app there is no voice engine (browser dev without the mock)", () => {
    expect(voiceBackend()).toBeNull();
  });

  test("first enable: models missing -> download with progress -> ready and listening for the name", async () => {
    const mock = createVoiceMock("fresh");
    let model: VoiceModel = { ...INITIAL_VOICE };
    const seen: VoiceEvent[] = [];
    mock.subscribe((e) => {
      seen.push(e);
      model = voiceReduce(model, { ...e, at: Date.now() });
    });
    model = voiceReduce(model, { type: "status", status: await mock.status() });
    expect(model.state).toBe("off");
    model = voiceReduce(model, { type: "status", status: await mock.configure({ ...model.status!.config, enabled: true, keyword: "Mochi" }) });
    expect(model.state).toBe("missing_models");
    await mock.download("default");
    expect(model.state).toBe("downloading");
    await until(() => seen.some((e) => e.type === "download"));
    expect(model.download?.models).toBe("default");
    await until(() => model.state === "wake_listening");
    expect(model.download).toBeNull();
    expect((await mock.status()).models.default.installed).toBe(true);
  });

  test("a cancelled download leaves the models missing", async () => {
    const mock = createVoiceMock("fresh");
    await mock.configure({ ...(await mock.status()).config, enabled: true });
    await mock.download("default");
    await mock.cancelDownload();
    const st = await mock.status();
    expect(st.state).toBe("missing_models");
    expect(st.models.default.installed).toBe(false);
  });

  test("push-to-talk: hearing with levels -> transcribing -> one transcript -> back to listening; muted never listens", async () => {
    const mock = createVoiceMock("ready");
    let model: VoiceModel = voiceReduce(INITIAL_VOICE, { type: "status", status: await mock.status() });
    const transcripts: string[] = [];
    mock.subscribe((e) => {
      model = voiceReduce(model, { ...e, at: Date.now() });
      if (e.type === "transcript") transcripts.push(e.id);
    });
    await mock.listen("start");
    expect(model.state).toBe("hearing");
    await until(() => model.level > 0);
    await mock.listen("stop");
    expect(model.state).toBe("transcribing");
    expect(model.level).toBe(0);
    await until(() => model.state === "wake_listening");
    expect(transcripts).toHaveLength(1);
    await mock.configure({ ...model.status!.config, muted: true });
    expect(model.state).toBe("muted");
    await mock.listen("start");
    expect(model.state).toBe("muted");
  });
});
