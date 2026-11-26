// "Open folder" (Settings > Assistant): show a folder in Explorer, Finder or the file manager through the app's
// `open_folder` command. Outside the app, or when the command is missing (an older app build), the path is copied
// to the clipboard instead, so the button always does something useful.
import { copyText } from "./pairing";
import { isTauri, native, OPEN_FOLDER_ERRORS } from "./native";

export interface OpenFolderDeps {
  tauri: boolean;
  open(path: string): Promise<void>;
  copy(text: string): Promise<boolean>;
}

const DEFAULT_DEPS: OpenFolderDeps = { tauri: isTauri, open: (p) => native.openFolder(p), copy: copyText };

function messageOf(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return "";
}

/**
 * Open `path`; resolves with a line to show (a toast), or null when the folder opened. `missing` words the "doesn't
 * exist" case (the assistant's workspace is made the first time it works).
 */
export async function openFolder(path: string, opts: { missing?: string } = {}, deps: OpenFolderDeps = DEFAULT_DEPS): Promise<string | null> {
  const copied = async () => ((await deps.copy(path)) ? "Copied the folder path" : "Couldn't open or copy the folder path.");
  if (!deps.tauri) return copied();
  try {
    await deps.open(path);
    return null;
  } catch (err) {
    const msg = messageOf(err).trim();
    if (msg === "That folder doesn't exist." && opts.missing) return opts.missing;
    if (OPEN_FOLDER_ERRORS.includes(msg)) return msg;
    // Not one of the command's own answers: the command isn't there (an older app build).
    return copied();
  }
}
