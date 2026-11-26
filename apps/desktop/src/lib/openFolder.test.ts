import { describe, expect, test } from "bun:test";
import { openFolder, type OpenFolderDeps } from "./openFolder";

function deps(over: Partial<OpenFolderDeps> & { fail?: unknown } = {}): OpenFolderDeps & { opened: string[]; copied: string[] } {
  const opened: string[] = [];
  const copied: string[] = [];
  return {
    tauri: true,
    open: async (p) => {
      if (over.fail !== undefined) throw over.fail;
      opened.push(p);
    },
    copy: async (t) => {
      copied.push(t);
      return true;
    },
    ...over,
    opened,
    copied,
  };
}

const WS = "C:\\Users\\you\\Pawl";

describe("Open folder", () => {
  test("opens the folder: nothing to say", async () => {
    const d = deps();
    expect(await openFolder(WS, {}, d)).toBeNull();
    expect(d.opened).toEqual([WS]);
    expect(d.copied).toEqual([]);
  });

  test("the command's own answers are shown as-is; a missing workspace gets its own line", async () => {
    expect(await openFolder(WS, {}, deps({ fail: "That's a file, not a folder." }))).toBe("That's a file, not a folder.");
    expect(await openFolder("\\\\server\\share", {}, deps({ fail: "Use a folder on this computer." }))).toBe("Use a folder on this computer.");
    expect(await openFolder(WS, { missing: "~/Pawl doesn't exist yet." }, deps({ fail: "That folder doesn't exist." }))).toBe("~/Pawl doesn't exist yet.");
    expect(await openFolder(WS, {}, deps({ fail: "That folder doesn't exist." }))).toBe("That folder doesn't exist.");
  });

  test("an older app without the command, or outside the app: the path is copied instead", async () => {
    const old = deps({ fail: "Command open_folder not found" });
    expect(await openFolder(WS, {}, old)).toBe("Copied the folder path");
    expect(old.copied).toEqual([WS]);
    const web = deps({ tauri: false });
    expect(await openFolder(WS, {}, web)).toBe("Copied the folder path");
    expect(web.opened).toEqual([]);
    const noClipboard = deps({ tauri: false, copy: async () => false });
    expect(await openFolder(WS, {}, noClipboard)).toBe("Couldn't open or copy the folder path.");
  });
});
