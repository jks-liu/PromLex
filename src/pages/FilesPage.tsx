import { Ban, Download, ExternalLink, FileText, FolderOpen, LoaderCircle, RefreshCw, UploadCloud } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, isDesktopApp } from "../api";
import { TranslationOptions, type TranslationOptionsValue } from "../components/TranslationOptions";
import { downloadBlob, saveBlobAs } from "../utils";
import type { BootstrapData, FileJobStatus, FileOutputMode } from "../types";

export function FilesPage({ data, onReload }: { data: BootstrapData; onReload: () => Promise<void> }) {
  const { t } = useTranslation();
  const inputRef = useRef<HTMLInputElement>(null);
  const defaultProvider = useMemo(() => data.providers.find((provider) => provider.enabled)?.id || "", [data.providers]);
  const [options, setOptions] = useState<TranslationOptionsValue>(() => readOptions(defaultProvider, data.prompts[0]?.id || ""));
  const [outputMode, setOutputMode] = useState<FileOutputMode>(() => readOutputMode());
  const [summarize, setSummarize] = useState(() => readSummarize());
  const [file, setFile] = useState<File | null>(null);
  const [sourcePath, setSourcePath] = useState("");
  const [jobs, setJobs] = useState<FileJobStatus[]>([]);
  const [submitting, setSubmitting] = useState(false);
  const [downloadingId, setDownloadingId] = useState("");
  const [retryingId, setRetryingId] = useState("");
  const [cancellingId, setCancellingId] = useState("");
  const [browserDownloadedIds, setBrowserDownloadedIds] = useState<Set<string>>(() => new Set());
  const [error, setError] = useState("");
  const knownStates = useRef(new Map<string, FileJobStatus["state"]>());
  const autoDownloadedIds = useRef(readAutoDownloadedIds());
  const autoDownloadingIds = useRef(new Set<string>());

  useEffect(() => {
    localStorage.setItem("tranova-file-summarize", String(summarize));
  }, [summarize]);

  const choose = useCallback((next: File | undefined, nextSourcePath = "") => {
    if (next) {
      setFile(next);
      setSourcePath(nextSourcePath);
      setError("");
    }
  }, []);

  const downloadJob = useCallback(async (job: FileJobStatus, saveAs = false): Promise<boolean> => {
    if (job.state !== "completed") return false;
    setDownloadingId(job.id);
    setError("");
    try {
      if (isDesktopApp()) {
        const destination = await resolveDownloadPath(job, data.settings, t(saveAs ? "files.saveAs" : "files.download"), saveAs);
        if (!destination) return false;
        const saved = await api.saveFileJob(job.id, destination);
        setJobs((current) => current.map((item) => item.id === saved.id ? saved : item));
        await onReload();
      } else {
        if (saveAs) {
          if (!await saveBlobAs(job.resultFilename, () => api.downloadFileJob(job.id), t("files.saveAsFilename"))) return false;
        } else {
          downloadBlob(job.resultFilename, await api.downloadFileJob(job.id));
        }
        setBrowserDownloadedIds((current) => new Set(current).add(job.id));
      }
      autoDownloadedIds.current.add(job.id);
      rememberAutoDownloadedJob(job.id);
      return true;
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
      return false;
    } finally {
      setDownloadingId("");
    }
  }, [data.settings, onReload, t]);

  const queueAutoDownload = useCallback((next: FileJobStatus[]) => {
    if (!data.settings.autoDownloadFiles || autoDownloadingIds.current.size > 0) return;
    const job = next.find((item) => item.state === "completed"
      && !item.downloadedPath
      && !autoDownloadedIds.current.has(item.id));
    if (!job) return;
    autoDownloadingIds.current.add(job.id);
    void downloadJob(job)
      .then((success) => {
        // Avoid prompting repeatedly after a cancelled or failed automatic download.
        if (!success) autoDownloadedIds.current.add(job.id);
      })
      .finally(() => autoDownloadingIds.current.delete(job.id));
  }, [data.settings.autoDownloadFiles, downloadJob]);

  const syncJobs = useCallback(async () => {
    try {
      const next = await api.fileJobs();
      const firstLoad = knownStates.current.size === 0;
      const completedSinceLastRefresh = next.some((job) => job.state === "completed" && knownStates.current.has(job.id) && knownStates.current.get(job.id) !== "completed");
      for (const job of next) knownStates.current.set(job.id, job.state);
      setJobs(next);
      queueAutoDownload(next);
      if (completedSinceLastRefresh || (firstLoad && next.some((job) => job.state === "completed"))) void onReload();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, [onReload, queueAutoDownload]);

  const refreshJobs = syncJobs;

  useEffect(() => {
    const providerExists = data.providers.some((provider) => provider.enabled && provider.id === options.providerId);
    if (!providerExists && options.providerId !== defaultProvider) setOptions((current) => ({ ...current, providerId: defaultProvider }));
    if (options.promptId && !data.prompts.some((prompt) => prompt.id === options.promptId)) {
      setOptions((current) => ({ ...current, promptId: data.prompts[0]?.id || "" }));
    }
  }, [data.prompts, defaultProvider, options.providerId, options.promptId]);

  useEffect(() => {
    localStorage.setItem("tranova-file-options", JSON.stringify(options));
  }, [options]);

  useEffect(() => {
    localStorage.setItem("tranova-file-output-mode", outputMode);
  }, [outputMode]);

  useEffect(() => {
    if (!isDesktopApp()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void import("@tauri-apps/api/webview")
      .then(({ getCurrentWebview }) => getCurrentWebview().onDragDropEvent(async (event) => {
        if (event.payload.type !== "drop" || !event.payload.paths[0]) return;
        try {
          const dropped = await api.readDroppedFile(event.payload.paths[0]);
          if (disposed) return;
          const bytes = Uint8Array.from(atob(dropped.contentBase64), (character) => character.charCodeAt(0));
          choose(new File([bytes], dropped.name, { type: mimeTypeForName(dropped.name) }), event.payload.paths[0]);
        } catch (reason) {
          if (!disposed) setError(reason instanceof Error ? reason.message : String(reason));
        }
      }))
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch((reason) => {
        if (!disposed) setError(reason instanceof Error ? reason.message : String(reason));
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [choose]);

  useEffect(() => {
    void syncJobs();
    const timer = window.setInterval(() => void syncJobs(), 900);
    return () => window.clearInterval(timer);
  }, [syncJobs]);

  const chooseFile = async () => {
    if (!isDesktopApp()) {
      inputRef.current?.click();
      return;
    }
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({ directory: false, multiple: false });
      if (typeof selected !== "string") return;
      const dropped = await api.readDroppedFile(selected);
      const bytes = Uint8Array.from(atob(dropped.contentBase64), (character) => character.charCodeAt(0));
      choose(new File([bytes], dropped.name, { type: mimeTypeForName(dropped.name) }), selected);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const translate = async () => {
    if (!file || !options.providerId) return;
    setSubmitting(true);
    setError("");
    try {
      const job = await api.translateFile(file, {
        ...options,
        promptId: options.promptId || undefined,
        outputMode,
        summarize,
      }, sourcePath || undefined);
      knownStates.current.set(job.id, job.state);
      setJobs((current) => [job, ...current.filter((item) => item.id !== job.id)]);
      setFile(null);
      setSourcePath("");
      if (inputRef.current) inputRef.current.value = "";
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSubmitting(false);
    }
  };

  const cancelJob = async (job: FileJobStatus) => {
    if (job.state !== "queued" && job.state !== "processing") return;
    setCancellingId(job.id);
    setError("");
    try {
      const next = await api.cancelFileJob(job.id);
      setJobs((current) => current.map((item) => item.id === next.id ? next : item));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setCancellingId("");
    }
  };

  const retryFailedBatches = async (job: FileJobStatus) => {
    if (job.state !== "failed" && (job.state !== "completed" || job.failedBatches.length === 0)) return;
    setRetryingId(job.id);
    setError("");
    try {
      const next = await api.retryFileJob(job.id);
      setJobs((current) => current.map((item) => item.id === next.id ? next : item));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setRetryingId("");
    }
  };

  const openDownloadedFile = async (job: FileJobStatus) => {
    if (!isDesktopApp() || !job.downloadedPath) return;
    try {
      await api.openPath(job.downloadedPath);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const openDownloadedFolder = async (job: FileJobStatus) => {
    if (!isDesktopApp() || !job.downloadedPath) return;
    try {
      const { dirname } = await import("@tauri-apps/api/path");
      await api.openPath(await dirname(job.downloadedPath));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const changeOptions = (next: TranslationOptionsValue) => {
    setOptions(next);
    localStorage.setItem("tranova-file-options", JSON.stringify(next));
  };

  return (
    <section className="page files-page">
      <div className="page-heading"><div><h1>{t("files.title")}</h1><p>{t("files.subtitle")}</p></div></div>
      <TranslationOptions value={options} onChange={changeOptions} providers={data.providers} prompts={data.prompts} glossaries={data.glossaries} />
      <div className="file-output-options">
        <label>
          <span>{t("files.mode")}</span>
          <select value={outputMode} onChange={(event) => setOutputMode(event.target.value as FileOutputMode)}>
            <option value="translated">{t("files.translatedOnly")}</option>
            <option value="bilingual">{t("files.bilingual")}</option>
          </select>
        </label>
        <label className="check-row file-summary-toggle">
          <input type="checkbox" checked={summarize} onChange={(event) => setSummarize(event.target.checked)} />
          <span>{t("files.summarize")}</span>
        </label>
      </div>
      <button
        className={`drop-zone ${file ? "has-file" : ""}`}
        type="button"
        onClick={() => void chooseFile()}
        onDragOver={(event) => event.preventDefault()}
        onDrop={(event) => { event.preventDefault(); if (!isDesktopApp()) choose(event.dataTransfer.files[0]); }}
      >
        {file ? <FileText size={34} /> : <UploadCloud size={34} />}
        <strong>{file?.name || t("files.drop")}</strong>
        <span>{file ? `${(file.size / 1024).toFixed(1)} KB` : t("files.supported")}</span>
      </button>
      <input ref={inputRef} className="visually-hidden" type="file" accept=".pdf,.docx,.pptx,.xlsx,.txt,.md,.markdown,.html,.htm,.csv,.json,.srt,.vtt,.png,.jpg,.jpeg,.webp" onChange={(event) => choose(event.target.files?.[0])} />
      {error && <div className="alert error-alert">{error}</div>}
      <div className="action-row">
        <button className="primary-button" onClick={translate} disabled={!file || submitting || !options.providerId}>
          <FileText size={18} /> {submitting ? t("files.working") : t("files.action")}
        </button>
        <button className="icon-text-button" onClick={() => void refreshJobs()} title={t("files.refresh")} aria-label={t("files.refresh")}>
          <RefreshCw size={17} /> {t("files.refresh")}
        </button>
      </div>
      <section className="file-jobs-section">
        <header className="section-heading"><h2>{t("files.jobs")}</h2></header>
        {jobs.length === 0 ? <div className="empty-state">{t("files.noJobs")}</div> : (
          <div className="file-job-list">
            {jobs.map((job) => {
              const percent = progressPercent(job);
              return (
                <article className={`file-job ${job.state}`} key={job.id}>
                  <div className="file-job-heading">
                    <div className="file-job-name"><strong>{job.filename}</strong><span>{job.outputMode === "bilingual" ? t("files.bilingual") : t("files.translatedOnly")}</span></div>
                    <span className={`badge ${job.state === "completed" ? "enabled" : ""}`}>{t(`files.${job.state}`)}</span>
                  </div>
                  <div className="progress-track" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent}>
                    <div className="progress-fill" style={{ width: `${percent}%` }} />
                  </div>
                  <div className="file-job-meta">
                    <span>{t(`files.${job.stage}`)}</span>
                    <span>{t("files.segments", { done: job.translatedSegments, total: job.totalSegments })}</span>
                    <span>{t("files.batches", { done: job.completedBatches, total: job.totalBatches })}</span>
                    {job.skippedSegments > 0 && <span>{t("files.skipped", { count: job.skippedSegments })}</span>}
                  </div>
                  {job.streamingText && job.state === "processing" && <div className="file-job-stream"><strong>{t("files.streaming")}</strong><pre>{job.streamingText}</pre></div>}
                  {(job.state === "failed" || job.failedBatches.length > 0) && <div className="file-job-partial"><span>{job.state === "failed" ? t("files.retryFileHint") : t("files.partialWarning", { count: job.failedSegments })}</span><button className="icon-text-button compact" onClick={() => void retryFailedBatches(job)} disabled={(job.state !== "failed" && job.state !== "completed") || retryingId === job.id}><RefreshCw size={15} className={retryingId === job.id ? "spin-icon" : undefined} /> {retryingId === job.id ? t("files.retrying") : job.state === "failed" ? t("files.retryFile") : t("files.retryFailed")}</button></div>}
                  {job.error && <div className="file-job-error">{job.error}</div>}
                  {(job.state === "queued" || job.state === "processing") && <div className="file-job-actions file-job-cancel"><span className="muted">{job.state === "queued" ? t("files.waitingForQueue") : t(`files.${job.stage}`)}</span><button className="icon-text-button compact" onClick={() => void cancelJob(job)} disabled={cancellingId === job.id}><Ban size={15} /> {cancellingId === job.id ? t("files.cancelling") : t("files.cancel")}</button></div>}
                  {job.state === "completed" && (
                    <div className="file-job-actions">
                      <div className="download-info">
                        {downloadingId === job.id ? <span className="download-status"><LoaderCircle size={15} className="spin-icon" /> {t("files.downloading")}</span> : (job.downloadedPath || browserDownloadedIds.has(job.id)) ? <span className="download-status complete">{t("files.downloadComplete")}</span> : <span className="muted">{job.resultFilename}</span>}
                        {job.downloadedPath && <span className="download-path" title={job.downloadedPath}>{t("files.savedTo", { path: job.downloadedPath })}</span>}
                      </div>
                      <div className="download-actions">
                        <button className="secondary-button" onClick={() => void downloadJob(job, true)} disabled={downloadingId === job.id}>
                          <Download size={18} /> {t("files.saveAs")}
                        </button>
                        <button className="secondary-button" onClick={() => void downloadJob(job)} disabled={downloadingId === job.id}>
                          <Download size={18} /> {downloadingId === job.id ? t("files.downloading") : t("files.download")}
                        </button>
                        {isDesktopApp() && job.downloadedPath && <>
                          <button className="icon-text-button compact" onClick={() => void openDownloadedFile(job)} title={t("files.openFile")}>
                            <ExternalLink size={16} /> {t("files.openFile")}
                          </button>
                          <button className="icon-text-button compact" onClick={() => void openDownloadedFolder(job)} title={t("files.openFolder")}>
                            <FolderOpen size={16} /> {t("files.openFolder")}
                          </button>
                        </>}
                      </div>
                    </div>
                  )}
                </article>
              );
            })}
          </div>
        )}
      </section>
    </section>
  );
}

function progressPercent(job: FileJobStatus) {
  if (job.state === "completed") return 100;
  if (job.totalSegments > 0) {
    const streamingSegments = Math.min(job.streamingSegments, job.streamingBatchSegments);
    return Math.min(99, Math.round(((job.translatedSegments + streamingSegments) / job.totalSegments) * 100));
  }
  return job.state === "processing" ? 4 : 0;
}

function readOptions(defaultProvider: string, defaultPrompt: string): TranslationOptionsValue {
  const fallback: TranslationOptionsValue = {
    sourceLanguage: "auto",
    targetLanguage: "zh-CN",
    providerId: defaultProvider,
    promptId: defaultPrompt,
    additionalPrompt: "",
    glossaryIds: [],
    reasoningEffort: "none",
  };
  try {
    const stored = JSON.parse(localStorage.getItem("tranova-file-options") || "null") as Partial<TranslationOptionsValue> | null;
    if (!stored || typeof stored !== "object") return fallback;
    const sourceLanguage = stored.sourceLanguage === "zh" ? "zh-CN" : stored.sourceLanguage;
    const targetLanguage = stored.targetLanguage === "zh" ? "zh-CN" : stored.targetLanguage;
    return {
      ...fallback,
      ...stored,
      ...(sourceLanguage ? { sourceLanguage } : {}),
      ...(targetLanguage ? { targetLanguage } : {}),
      glossaryIds: Array.isArray(stored.glossaryIds) ? stored.glossaryIds : [],
      reasoningEffort: stored.reasoningEffort === "none" || stored.reasoningEffort === "low" || stored.reasoningEffort === "medium" || stored.reasoningEffort === "high" ? stored.reasoningEffort : "none",
    };
  } catch {
    return fallback;
  }
}

function readOutputMode(): FileOutputMode {
  return localStorage.getItem("tranova-file-output-mode") === "bilingual" ? "bilingual" : "translated";
}

function readSummarize() {
  return localStorage.getItem("tranova-file-summarize") !== "false";
}

async function resolveDownloadPath(
  job: FileJobStatus,
  settings: BootstrapData["settings"],
  title: string,
  saveAs = false,
): Promise<string | null> {
  const { dirname, downloadDir, join } = await import("@tauri-apps/api/path");
  let directory = "";
  if (settings.downloadLocation === "source" && job.sourcePath) {
    directory = await dirname(job.sourcePath);
  } else if (settings.downloadLocation === "custom" && settings.customDownloadDirectory) {
    directory = settings.customDownloadDirectory;
  } else {
    directory = await downloadDir();
  }
  if (!directory) directory = await downloadDir();
  if (!saveAs && !settings.askDownloadLocation) return join(directory, job.resultFilename);

  const defaultDirectory = settings.lastDownloadDirectory || directory;
  const { save } = await import("@tauri-apps/plugin-dialog");
  return (await save({
    title,
    defaultPath: await join(defaultDirectory, job.resultFilename),
  })) || null;
}

function readAutoDownloadedIds() {
  try {
    const value = JSON.parse(localStorage.getItem("tranova-auto-downloaded-jobs") || "[]");
    return new Set<string>(Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : []);
  } catch {
    return new Set<string>();
  }
}

function rememberAutoDownloadedJob(id: string) {
  try {
    const ids = readAutoDownloadedIds();
    ids.add(id);
    localStorage.setItem("tranova-auto-downloaded-jobs", JSON.stringify([...ids].slice(-100)));
  } catch {
    // Local storage can be unavailable in a restricted browser context.
  }
}

function mimeTypeForName(filename: string) {
  const extension = filename.split(".").pop()?.toLowerCase();
  const types: Record<string, string> = {
    docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    pdf: "application/pdf",
    pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    txt: "text/plain",
    md: "text/markdown",
    markdown: "text/markdown",
    html: "text/html",
    htm: "text/html",
    csv: "text/csv",
    json: "application/json",
  };
  return (extension && types[extension]) || "application/octet-stream";
}
