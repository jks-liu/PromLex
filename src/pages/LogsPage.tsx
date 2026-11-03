import { Bug, ChevronLeft, ChevronRight, Clock3, MessageSquareText, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api";
import type { AiConversationLog, BootstrapData, LogKind, LogPage, SystemLogEntry } from "../types";

const PAGE_SIZE = 20;

export function LogsPage({ data }: { data: BootstrapData }) {
  const { t, i18n } = useTranslation();
  const [activeTab, setActiveTab] = useState<LogKind>("system");
  const [systemPage, setSystemPage] = useState<LogPage<SystemLogEntry>>(emptyPage());
  const [conversationPages, setConversationPages] = useState({ text: emptyPage<AiConversationLog>(), file: emptyPage<AiConversationLog>() });
  const [pages, setPages] = useState<Record<LogKind, number>>({ system: 1, text: 1, file: 1 });
  const [beforeDate, setBeforeDate] = useState("");
  const [error, setError] = useState("");

  const load = useCallback(async (kind: LogKind, page: number) => {
    try {
      const result = await api.logs(kind, page, PAGE_SIZE);
      if (kind === "system") setSystemPage(result as LogPage<SystemLogEntry>);
      else setConversationPages((current) => ({ ...current, [kind]: result as LogPage<AiConversationLog> }));
      setError("");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, []);

  useEffect(() => {
    if (!data.settings.loggingEnabled) return;
    void load(activeTab, pages[activeTab]);
    const timer = window.setInterval(() => void load(activeTab, pages[activeTab]), 1_500);
    return () => window.clearInterval(timer);
  }, [activeTab, data.settings.loggingEnabled, load, pages]);

  const page = activeTab === "system" ? systemPage : conversationPages[activeTab];

  const remove = async (id: string) => {
    try {
      await api.deleteLog(activeTab, id);
      const nextPage = page.items.length === 1 && pages[activeTab] > 1 ? pages[activeTab] - 1 : pages[activeTab];
      setPages((current) => ({ ...current, [activeTab]: nextPage }));
      await load(activeTab, nextPage);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const clear = async (before?: number) => {
    if (!window.confirm(t(before === undefined ? "logs.confirmClear" : "logs.confirmClearBefore"))) return;
    try {
      await api.clearLogs(activeTab, before);
      setPages((current) => ({ ...current, [activeTab]: 1 }));
      await load(activeTab, 1);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const selectTab = (kind: LogKind) => {
    setActiveTab(kind);
    setBeforeDate("");
  };

  return (
    <section className="page logs-page">
      <div className="page-heading"><div><h1>{t("logs.title")}</h1><p>{t("logs.subtitle")}</p></div></div>
      <div className="log-tabs" role="tablist" aria-label={t("logs.title")}>
        <button role="tab" aria-selected={activeTab === "system"} className={activeTab === "system" ? "active" : ""} onClick={() => selectTab("system")}><Bug size={17} /> {t("logs.system")}</button>
        {(["text", "file"] as const).map((kind) => <button key={kind} role="tab" aria-selected={activeTab === kind} className={activeTab === kind ? "active" : ""} onClick={() => selectTab(kind)}><MessageSquareText size={17} /> {t(`logs.${kind}`)}</button>)}
      </div>
      <section className="logs-section" role="tabpanel">
        <div className="log-toolbar">
          <div className="log-date-action">
            <label><span>{t("logs.beforeDate")}</span><input type="date" value={beforeDate} onChange={(event) => setBeforeDate(event.target.value)} /></label>
            <button className="secondary-button" disabled={!beforeDate} onClick={() => void clear(new Date(`${beforeDate}T00:00:00`).getTime())}>{t("logs.clearBefore")}</button>
          </div>
          <button className="secondary-button danger-button" disabled={page.total === 0} onClick={() => void clear()}><Trash2 size={16} /> {t("logs.clearAll")}</button>
        </div>
        {error && <div className="alert error-alert">{error}</div>}
        {page.items.length === 0 ? <div className="empty-state">{t("logs.empty")}</div> : activeTab === "system" ? (
          <div className="log-list">{(page.items as SystemLogEntry[]).map((entry) => <SystemLogRow key={entry.id} entry={entry} locale={i18n.language} onDelete={() => void remove(entry.id)} />)}</div>
        ) : activeTab === "file" ? (
          <FileConversationGroups entries={page.items as AiConversationLog[]} locale={i18n.language} onDelete={(id) => void remove(id)} />
        ) : (
          <div className="conversation-list">{(page.items as AiConversationLog[]).map((entry) => <ConversationRow key={entry.id} entry={entry} locale={i18n.language} onDelete={() => void remove(entry.id)} />)}</div>
        )}
        <div className="pagination">
          <button className="icon-button" aria-label={t("logs.previous")} disabled={page.page <= 1} onClick={() => setPages((current) => ({ ...current, [activeTab]: current[activeTab] - 1 }))}><ChevronLeft size={18} /></button>
          <span>{t(activeTab === "file" ? "logs.jobPage" : "logs.page", { page: page.page, total: Math.max(1, page.totalPages), count: page.total })}</span>
          <button className="icon-button" aria-label={t("logs.next")} disabled={page.page >= page.totalPages} onClick={() => setPages((current) => ({ ...current, [activeTab]: current[activeTab] + 1 }))}><ChevronRight size={18} /></button>
        </div>
      </section>
    </section>
  );
}

function emptyPage<T>(): LogPage<T> {
  return { items: [], page: 1, pageSize: PAGE_SIZE, total: 0, totalPages: 0 };
}

function timestamp(value: string, locale: string) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return value;
  return new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeStyle: "medium" }).format(new Date(parsed));
}

function SystemLogRow({ entry, locale, onDelete }: { entry: SystemLogEntry; locale: string; onDelete: () => void }) {
  const { t } = useTranslation();
  return <article className={`system-log-row log-${entry.level}`}><span className="log-time"><Clock3 size={14} /> {timestamp(entry.timestamp, locale)}</span><strong>{entry.scope}</strong><span className="log-message">{entry.message}</span><span className="badge">{entry.level}</span><button className="icon-button danger" onClick={onDelete} title={t("logs.deleteOne")} aria-label={t("logs.deleteOne")}><Trash2 size={15} /></button></article>;
}

function ConversationRow({ entry, locale, onDelete }: { entry: AiConversationLog; locale: string; onDelete: () => void }) {
  const { t } = useTranslation();
  return <article className={`conversation-row ${entry.success ? "success" : "failure"}`}><div className="conversation-meta"><strong>{entry.operation}</strong><span>{timestamp(entry.timestamp, locale)}</span><span>{entry.provider} Â· {entry.model}</span><span>{entry.durationMs} ms</span><span className={`badge ${entry.success ? "enabled" : ""}`}>{entry.success ? t("logs.success") : t("logs.failure")}</span><button className="icon-button danger log-delete" onClick={onDelete} title={t("logs.deleteOne")} aria-label={t("logs.deleteOne")}><Trash2 size={15} /></button></div><div className="conversation-columns"><div><strong>{t("logs.request")}</strong><pre>{entry.request}</pre></div><div><strong>{t("logs.response")}</strong><pre>{entry.error || entry.response}</pre></div></div></article>;
}

function FileConversationGroups({ entries, locale, onDelete }: { entries: AiConversationLog[]; locale: string; onDelete: (id: string) => void }) {
  const { t } = useTranslation();
  const groups = new Map<string, AiConversationLog[]>();
  for (const entry of entries) {
    const key = entry.jobId || "legacy";
    const group = groups.get(key) || [];
    group.push(entry);
    groups.set(key, group);
  }
  return <div className="conversation-list">{Array.from(groups, ([id, logs]) => (
    <details className="file-log-group" key={id}>
      <summary><strong>{logs[0].filename || t("logs.legacyJob")}</strong> · {t("logs.entryCount", { count: logs.length })}<span className="file-log-id">{id === "legacy" ? t("logs.legacyNote") : id}</span></summary>
      <div className="conversation-list">{logs.map((entry) => <ConversationRow key={entry.id} entry={entry} locale={locale} onDelete={() => onDelete(entry.id)} />)}</div>
    </details>
  ))}</div>;
}
