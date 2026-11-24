/**
 * Minimal Gemini Interactions API client for image generation
 * (POST /v1beta/interactions, docs: ai.google.dev/gemini-api/docs/image-generation).
 *
 * The API key is read from the repo-root .env and only ever placed in the
 * `x-goog-api-key` request header. It is never logged, echoed or written anywhere.
 */
import { readFile } from "node:fs/promises";
import { API_REVISION, API_URL, ENV_PATH, MODEL, type ImageSize } from "./config.ts";

let cachedKey: string | undefined;

async function apiKey(): Promise<string> {
  if (cachedKey) return cachedKey;
  let text = "";
  try {
    text = await readFile(ENV_PATH, "utf8");
  } catch {
    // fall through to the process environment
  }
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const m = /^(?:export\s+)?GEMINI_API_KEY\s*=\s*(.*)$/.exec(line);
    if (!m) continue;
    let v = (m[1] ?? "").trim();
    if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) v = v.slice(1, -1);
    if (v) cachedKey = v;
  }
  cachedKey ??= process.env.GEMINI_API_KEY || undefined;
  if (!cachedKey) throw new Error(`GEMINI_API_KEY not found in ${ENV_PATH} or the environment`);
  return cachedKey;
}

export interface RefImage {
  data: Buffer;
  mime: string;
}

export interface GenerateRequest {
  prompt: string;
  refs?: RefImage[];
  aspectRatio: string;
  imageSize: ImageSize;
  /** Request timeout per attempt, ms. */
  timeoutMs?: number;
  maxAttempts?: number;
}

export interface GenerateResult {
  data: Buffer;
  mime: string;
  text: string;
  interactionId?: string;
  usage?: unknown;
}

interface ContentBlock {
  type?: string;
  text?: string;
  data?: string;
  mime_type?: string;
  uri?: string;
}
interface InteractionResponse {
  id?: string;
  status?: string;
  steps?: { type?: string; content?: ContentBlock[] }[];
  outputs?: ContentBlock[];
  usage?: unknown;
  error?: { code?: number; message?: string; status?: string };
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Strip anything that could echo a credential before surfacing an API error. */
function scrub(s: string, key: string): string {
  return (key ? s.split(key).join("<redacted>") : s).slice(0, 2000);
}

export async function generateImage(req: GenerateRequest): Promise<GenerateResult> {
  const key = await apiKey();
  const input: object[] = [{ type: "text", text: req.prompt }];
  for (const r of req.refs ?? []) input.push({ type: "image", mime_type: r.mime, data: r.data.toString("base64") });
  const body = JSON.stringify({
    model: MODEL,
    input,
    response_format: { type: "image", aspect_ratio: req.aspectRatio, image_size: req.imageSize },
    store: false,
  });

  const maxAttempts = req.maxAttempts ?? 5;
  let lastErr: Error | undefined;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    let res: Response;
    try {
      res = await fetch(API_URL, {
        method: "POST",
        headers: {
          "x-goog-api-key": key,
          "Content-Type": "application/json",
          "Api-Revision": API_REVISION,
        },
        body,
        signal: AbortSignal.timeout(req.timeoutMs ?? 240_000),
      });
    } catch (e) {
      lastErr = new Error(`network error: ${scrub(String(e), key)}`);
      await sleep(backoff(attempt));
      continue;
    }
    const raw = await res.text();
    if (!res.ok) {
      lastErr = new Error(`HTTP ${res.status}: ${scrub(raw, key)}`);
      const retryable = res.status === 429 || res.status >= 500;
      if (!retryable) throw lastErr;
      const ra = Number(res.headers.get("retry-after"));
      await sleep(Number.isFinite(ra) && ra > 0 ? ra * 1000 : backoff(attempt));
      continue;
    }
    let json: InteractionResponse;
    try {
      json = JSON.parse(raw) as InteractionResponse;
    } catch {
      throw new Error(`invalid JSON response: ${scrub(raw, key)}`);
    }
    if (json.error) throw new Error(`API error: ${scrub(JSON.stringify(json.error), key)}`);

    const blocks: ContentBlock[] = [
      ...(json.steps ?? []).filter((s) => s.type === "model_output").flatMap((s) => s.content ?? []),
      ...(json.outputs ?? []),
    ];
    const images = blocks.filter((b) => b.type === "image" && b.data);
    const text = blocks.filter((b) => b.type === "text" && b.text).map((b) => b.text).join("\n");
    const img = images.at(-1);
    if (!img?.data) {
      // Usually a safety block or the model answering in text only: retry a couple of times.
      lastErr = new Error(`no image in response (status=${json.status ?? "?"}): ${text.slice(0, 400)}`);
      await sleep(backoff(attempt));
      continue;
    }
    return {
      data: Buffer.from(img.data, "base64"),
      mime: img.mime_type ?? "image/png",
      text,
      interactionId: json.id,
      usage: json.usage,
    };
  }
  throw lastErr ?? new Error("generation failed");
}

function backoff(attempt: number): number {
  return Math.min(60_000, 2_000 * 2 ** (attempt - 1)) + Math.floor(Math.random() * 1000);
}
