import { afterEach, describe, expect, it, vi } from "vitest";
import { nowIso, readJsonFile, saveBlobAs } from "./utils";

afterEach(() => vi.unstubAllGlobals());

describe("save as", () => {
  it("cancels without downloading when the picker is dismissed", async () => {
    const load = vi.fn();
    vi.stubGlobal("window", { showSaveFilePicker: vi.fn().mockRejectedValue(new DOMException("Cancelled", "AbortError")) });
    await expect(saveBlobAs("file.txt", load, "Filename")).resolves.toBe(false);
    expect(load).not.toHaveBeenCalled();
  });

  it("writes the downloaded content and closes the chosen file", async () => {
    const blob = new Blob(["translation"]);
    const writer = { write: vi.fn(), close: vi.fn(), abort: vi.fn() };
    const picker = vi.fn().mockResolvedValue({ createWritable: async () => writer });
    vi.stubGlobal("window", { showSaveFilePicker: picker });
    await expect(saveBlobAs("file.txt", async () => blob, "Filename")).resolves.toBe(true);
    expect(picker).toHaveBeenCalledWith({ suggestedName: "file.txt" });
    expect(writer.write).toHaveBeenCalledWith(blob);
    expect(writer.close).toHaveBeenCalledOnce();
  });

  it("aborts the writable file when saving fails", async () => {
    const failure = new Error("Disk full");
    const writer = { write: vi.fn().mockRejectedValue(failure), close: vi.fn(), abort: vi.fn().mockResolvedValue(undefined) };
    vi.stubGlobal("window", { showSaveFilePicker: async () => ({ createWritable: async () => writer }) });
    await expect(saveBlobAs("file.txt", async () => new Blob(), "Filename")).rejects.toThrow("Disk full");
    expect(writer.abort).toHaveBeenCalledOnce();
    expect(writer.close).not.toHaveBeenCalled();
  });
});

describe("JSON imports", () => {
  it("parses an exported JSON file", async () => {
    const file = new File([JSON.stringify([{ id: "one" }])], "export.json", { type: "application/json" });
    await expect(readJsonFile(file)).resolves.toEqual([{ id: "one" }]);
  });

  it("creates an ISO timestamp for saved records", () => {
    expect(new Date(nowIso()).toString()).not.toBe("Invalid Date");
  });
});
