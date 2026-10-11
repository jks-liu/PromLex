import type {
  AppSettings,
  BootstrapData,
  FileJobStatus,
  Glossary,
  HistoryEntry,
  PromptTemplate,
  Provider,
  ProviderModel,
  LogKind,
  LogPage,
  SystemLogEntry,
  AiConversationLog,
  TranslateRequest,
  TranslateFileOptions,
  TranslationResult,
} from "./types";

let apiBase = "";

export function isDesktopApp() {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function initializeApi() {
  if (import.meta.env.DEV || !isDesktopApp()) return;
  const { invoke } = await import("@tauri-apps/api/core");
  apiBase = await invoke<string>("server_url");
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${apiBase}/api${path}`, {
    ...init,
    headers: {
      ...(init?.body instanceof FormData ? {} : { "Content-Type": "application/json" }),
      ...init?.headers,
    },
  });

  if (!response.ok) {
    const body = await response.json().catch(() => null) as { error?: string } | null;
    throw new Error(body?.error || `${response.status} ${response.statusText}`);
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}

async function requestBlob(path: string): Promise<Blob> {
  const response = await fetch(`${apiBase}/api${path}`);
  if (!response.ok) {
    const body = await response.json().catch(() => null) as { error?: string } | null;
    throw new Error(body?.error || `${response.status} ${response.statusText}`);
  }
  return response.blob();
}

export const api = {
  bootstrap: () => request<BootstrapData>("/bootstrap"),
  history: () => request<HistoryEntry[]>("/history"),
  logs: <K extends LogKind>(kind: K, page = 1, pageSize = 20) =>
    request<LogPage<K extends "system" ? SystemLogEntry : AiConversationLog>>(`/logs?kind=${kind}&page=${page}&pageSize=${pageSize}`),
  deleteLog: (kind: LogKind, id: string) =>
    request<void>(`/logs/${kind}/${encodeURIComponent(id)}`, { method: "DELETE" }),
  clearLogs: (kind: LogKind, before?: number) =>
    request<void>(`/logs/${kind}${before === undefined ? "" : `?before=${before}`}`, { method: "DELETE" }),
  translate: (payload: TranslateRequest) =>
    request<TranslationResult>("/translate", { method: "POST", body: JSON.stringify(payload) }),
  translateFile: async (file: File, options: TranslateFileOptions, sourcePath?: string) => {
    const form = new FormData();
    form.append("file", file);
    form.append("options", JSON.stringify(options));
    if (sourcePath) form.append("sourcePath", sourcePath);
    return request<FileJobStatus>("/translate-file", { method: "POST", body: form });
  },
  fileJobs: () => request<FileJobStatus[]>("/file-jobs"),
  fileJob: (id: string) => request<FileJobStatus>(`/file-jobs/${encodeURIComponent(id)}`),
  cancelFileJob: (id: string) =>
    request<FileJobStatus>(`/file-jobs/${encodeURIComponent(id)}/cancel`, { method: "POST" }),
  retryFileJob: (id: string) =>
    request<FileJobStatus>(`/file-jobs/${encodeURIComponent(id)}/retry`, { method: "POST" }),
  downloadFileJob: (id: string) => requestBlob(`/file-jobs/${encodeURIComponent(id)}/download`),
  saveFileJob: async (id: string, destination: string) => {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<FileJobStatus>("save_file_job", { jobId: id, destination });
  },
  saveProvider: (provider: Provider) =>
    request<Provider>("/providers", { method: "PUT", body: JSON.stringify(provider) }),
  deleteProvider: (id: string) => request<void>(`/providers/${id}`, { method: "DELETE" }),
  testProvider: (id: string) => request<{ message: string }>(`/providers/${id}/test`, { method: "POST" }),
  discoverModels: (baseUrl: string, apiKey: string, proxyMode: Provider["proxyMode"]) =>
    request<ProviderModel[]>("/providers/models", { method: "POST", body: JSON.stringify({ baseUrl, apiKey, proxyMode }) }),
  saveGlossary: (glossary: Glossary) =>
    request<Glossary>("/glossaries", { method: "PUT", body: JSON.stringify(glossary) }),
  deleteGlossary: (id: string) => request<void>(`/glossaries/${id}`, { method: "DELETE" }),
  savePrompt: (prompt: PromptTemplate) =>
    request<PromptTemplate>("/prompts", { method: "PUT", body: JSON.stringify(prompt) }),
  deletePrompt: (id: string) => request<void>(`/prompts/${id}`, { method: "DELETE" }),
  saveSettings: (settings: AppSettings) =>
    request<AppSettings>("/settings", { method: "PUT", body: JSON.stringify(settings) }),
  importData: (kind: "glossaries" | "prompts", data: unknown) =>
    request<BootstrapData>(`/import/${kind}`, { method: "POST", body: JSON.stringify(data) }),
  deleteHistory: (id: string) => request<void>(`/history/${encodeURIComponent(id)}`, { method: "DELETE" }),
  clearHistory: () => request<void>("/history", { method: "DELETE" }),
  readDroppedFile: async (path: string) => {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<{ name: string; contentBase64: string }>("read_dropped_file", { path });
  },
  openPath: async (path: string) => {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<void>("open_path", { path });
  },
};

export async function translateStream(
  payload: TranslateRequest,
  onChunk: (text: string) => void,
): Promise<TranslationResult> {
  const response = await fetch(`${apiBase}/api/translate-stream`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "text/event-stream" },
    body: JSON.stringify(payload),
  });
  if (!response.ok) {
    const body = await response.json().catch(() => null) as { error?: string } | null;
    throw new Error(body?.error || `${response.status} ${response.statusText}`);
  }
  if (!response.body) throw new Error("Streaming is not supported by this browser");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let eventName = "message";
  let eventData = "";
  let result: TranslationResult | undefined;
  const consume = (line: string) => {
    if (line.startsWith("event:")) {
      eventName = line.slice(6).trim();
    } else if (line.startsWith("data:")) {
      eventData += line.slice(5).trimStart();
    } else if (!line && eventData) {
      const payload = JSON.parse(eventData) as { text?: string; result?: TranslationResult; error?: string };
      if (eventName === "delta" && typeof payload.text === "string") onChunk(payload.text);
      if (eventName === "done" && payload.result) result = payload.result;
      if (eventName === "error") throw new Error(payload.error || "Translation failed");
      eventName = "message";
      eventData = "";
    }
  };
  while (true) {
    const { done, value } = await reader.read();
    buffer += decoder.decode(value || new Uint8Array(), { stream: !done });
    let newline = buffer.indexOf("\n");
    while (newline >= 0) {
      consume(buffer.slice(0, newline).replace(/\r$/, ""));
      buffer = buffer.slice(newline + 1);
      newline = buffer.indexOf("\n");
    }
    if (done) break;
  }
  consume(buffer.replace(/\r$/, ""));
  if (eventData) consume("");
  if (!result) throw new Error("Translation stream ended without a result");
  return result;
}
