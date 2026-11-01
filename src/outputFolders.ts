import i18n from "./i18n";
import { api, isDesktopApp } from "./api";
import type { FileResumeInput, CompletedFileStatus } from "./types";

export type OutputFolder = { kind: "desktop"; path: string; name: string }
  | { kind: "browser"; handle: FileSystemDirectoryHandle; name: string };

const supportedDocument = /\.(pdf|docx|pptx|xlsx|txt|md|markdown|html|htm|csv|json|srt|vtt|png|jpg|jpeg|webp)$/i;

export function isArchive(name: string) {
  return /\.(zip|rar|7z|tar|tar\.gz|tgz|tar\.bz2|tbz2?|tar\.xz|txz|tar\.zst|tzst|tar\.lzma)$/i.test(name);
}

export async function chooseOutputFolder(unsupportedMessage: string): Promise<OutputFolder | null> {
  if (isDesktopApp()) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({ directory: true, multiple: false });
    return typeof path === "string" ? { kind: "desktop", path, name: path } : null;
  }
  const picker = (window as Window & { showDirectoryPicker?: (options: { mode: "readwrite" }) => Promise<FileSystemDirectoryHandle> }).showDirectoryPicker;
  if (!picker) throw new Error(unsupportedMessage);
  try {
    const handle = await picker.call(window, { mode: "readwrite" });
    return { kind: "browser", handle, name: handle.name };
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") return null;
    throw error;
  }
}

export async function readResumeInput(folder: OutputFolder): Promise<FileResumeInput> {
  if (folder.kind === "desktop") {
    const data = await api.readOutputDirectory(folder.path);
    const bytes = Uint8Array.from(atob(data.contentBase64), (character) => character.charCodeAt(0));
    return { archive: new File([bytes], "resume.zip", { type: "application/zip" }) };
  }
  const files: NonNullable<FileResumeInput["files"]> = [];
  let size = 0;
  let entries = 0;
  async function visit(handle: FileSystemDirectoryHandle, prefix: string) {
    const directory = handle as FileSystemDirectoryHandle & { values: () => AsyncIterable<FileSystemHandle> };
    for await (const child of directory.values()) {
      if (++entries > 10_000) throw new Error(i18n.t("files.outputEntryLimit"));
      const path = prefix + child.name;
      if (child.kind === "directory") await visit(child as FileSystemDirectoryHandle, `${path}/`);
      else if (supportedDocument.test(child.name)) {
        const file = await (child as FileSystemFileHandle).getFile();
        size += file.size;
        if (size > 75 * 1024 * 1024) throw new Error(i18n.t("files.outputSizeLimit"));
        if (file.size > 0) files.push({ path, file });
      }
    }
  }
  await visit(folder.handle, "");
  return { files };
}

export async function writeCompletedFile(handle: FileSystemDirectoryHandle, entry: CompletedFileStatus, blob: Blob, overwrite: boolean) {
  const parts = entry.path.split("/");
  if (parts.some((part) => !part || part === "." || part === ".." || /[\\:\0]/.test(part))) throw new Error(i18n.t("files.unsafeOutputPath"));
  let directory = handle;
  for (const part of parts.slice(0, -1)) directory = await directory.getDirectoryHandle(part, { create: true });
  const name = parts[parts.length - 1];
  if (!overwrite) {
    try {
      const existing = await directory.getFileHandle(name);
      if ((await existing.getFile()).size > 0) throw new Error(i18n.t("files.outputExists", { path: entry.path }));
    } catch (error) {
      if (!(error instanceof DOMException) || error.name !== "NotFoundError") throw error;
    }
  }
  const file = await directory.getFileHandle(name, { create: true });
  let writer: FileSystemWritableFileStream | undefined;
  try {
    writer = await file.createWritable();
    await writer.write(blob);
    await writer.close();
  } catch (error) {
    await writer?.abort().catch(() => {});
    if (!overwrite) await directory.removeEntry(name).catch(() => {});
    throw error;
  }
}
