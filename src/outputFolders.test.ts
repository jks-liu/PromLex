import { describe, expect, it, vi } from "vitest";
import { isArchive, readResumeInput, writeCompletedFile } from "./outputFolders";

vi.mock("./api", () => ({ api: {}, isDesktopApp: () => false }));
vi.mock("./i18n", () => ({ default: { t: (key: string) => key } }));

function memoryDirectory(initial: Record<string, string>, failWrites = false) {
  const files = new Map(Object.entries(initial));
  const directory = (prefix: string): FileSystemDirectoryHandle => ({
    name: prefix.split("/").filter(Boolean).pop() || "output",
    kind: "directory",
    async *values() {
      const children = new Set([...files.keys()].filter((key) => key.startsWith(prefix)).map((key) => key.slice(prefix.length).split("/")[0]));
      for (const name of children) {
        if (files.has(prefix + name)) yield await directory(prefix).getFileHandle(name);
        else yield directory(`${prefix}${name}/`);
      }
    },
    getDirectoryHandle: async (name: string) => directory(`${prefix}${name}/`),
    getFileHandle: async (name: string, options?: { create?: boolean }) => {
      const path = prefix + name;
      if (!files.has(path)) {
        if (!options?.create) throw new DOMException("Missing", "NotFoundError");
        files.set(path, "");
      }
      return {
        name, kind: "file", getFile: async () => new File([files.get(path)!], name),
        createWritable: async () => {
          let pending = "";
          return {
            write: async (blob: Blob) => { if (failWrites) throw new Error("disk full"); pending = await blob.text(); },
            close: async () => { files.set(path, pending); },
            abort: async () => {},
          };
        },
      };
    },
    removeEntry: async (name: string) => { files.delete(prefix + name); },
  } as unknown as FileSystemDirectoryHandle);
  return { handle: directory(""), files };
}

describe("output folders", () => {
  it("recognizes all supported archive names", () => {
    for (const name of ["a.ZIP", "a.rar", "a.7z", "a.tar", "a.tar.gz", "a.tgz", "a.tar.bz2", "a.tar.xz", "a.tar.zst", "a.tar.lzma"]) expect(isArchive(name)).toBe(true);
    expect(isArchive("a.docx")).toBe(false);
  });

  it("loads existing translations by relative path and excludes incomplete empty files", async () => {
    const { handle } = memoryDirectory({ "nested/notes.txt": "translated", "partial.txt": "", "archive.7z": "binary" });
    const result = await readResumeInput({ kind: "browser", handle, name: "output" });
    expect(result.files?.map((entry) => entry.path)).toEqual(["nested/notes.txt"]);
    expect(await result.files![0].file.text()).toBe("translated");
  });

  it("writes completed files and protects unrelated existing files", async () => {
    const { handle, files } = memoryDirectory({ "existing.txt": "keep" });
    const entry = { id: 0, path: "nested/new.txt", revision: 1, resumed: false };
    await writeCompletedFile(handle, entry, new Blob(["translated"]), false);
    expect(files.get(entry.path)).toBe("translated");
    await expect(writeCompletedFile(handle, { ...entry, path: "existing.txt" }, new Blob(["replace"]), false)).rejects.toThrow();
    expect(files.get("existing.txt")).toBe("keep");
    await expect(writeCompletedFile(handle, { ...entry, path: "../outside.txt" }, new Blob(["bad"]), false)).rejects.toThrow();
  });

  it("removes incomplete files after a failed write", async () => {
    const { handle, files } = memoryDirectory({}, true);
    await expect(writeCompletedFile(handle, { id: 0, path: "new.txt", revision: 1, resumed: false }, new Blob(["translated"]), false)).rejects.toThrow("disk full");
    expect(files.has("new.txt")).toBe(false);
  });
});
