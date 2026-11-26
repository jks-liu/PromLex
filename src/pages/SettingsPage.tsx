import { FolderOpen, Globe2, Save } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api, isDesktopApp } from "../api";
import { ConfigurationTransfer } from "../components/ConfigurationTransfer";
import type { AppSettings, BootstrapData, DownloadLocation } from "../types";

export function SettingsPage({ data, onReload }: { data: BootstrapData; onReload: () => Promise<void> }) {
  const { t, i18n } = useTranslation();
  const [settings, setSettings] = useState<AppSettings>({ ...data.settings });
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");

  const applySettings = async (saved: AppSettings) => {
    setSettings(saved);
    localStorage.setItem("tranova-language", saved.language);
    await i18n.changeLanguage(saved.language);
    await onReload();
  };

  const save = async () => {
    setMessage(""); setError("");
    try {
      await applySettings(await api.saveSettings(settings));
      setMessage(t("status.saved"));
    } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
  };

  const chooseCustomDirectory = async () => {
    if (!isDesktopApp()) return;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected === "string") {
        setSettings((current) => ({ ...current, customDownloadDirectory: selected }));
      }
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  return (
    <section className="page settings-page">
      <div className="page-heading">
        <div><h1>{t("settings.title")}</h1><p>{t("settings.subtitle")}</p></div>
        <ConfigurationTransfer data={data} scope="configuration" onImported={(imported) => {
          setError("");
          setMessage("");
          return applySettings(imported.settings);
        }} />
      </div>
      {message && <div className="alert success-alert">{message}</div>}
      {error && <div className="alert error-alert">{error}</div>}
      <div className="settings-section">
        <h2>{t("settings.language")}</h2>
        <label className="setting-row"><div><strong>{t("settings.language")}</strong></div><select value={settings.language} onChange={(event) => setSettings({ ...settings, language: event.target.value as AppSettings["language"] })}><option value="en">English</option><option value="zh-CN">简体中文</option></select></label>
      </div>
      <div className="settings-section">
        <h2>{t("settings.proxy")}</h2>
        <label className="setting-row"><div><strong>{t("settings.proxy")}</strong><span>{t("settings.proxyHint")}</span></div><input value={settings.proxyUrl} onChange={(event) => setSettings({ ...settings, proxyUrl: event.target.value })} placeholder="socks5h://127.0.0.1:1080" /></label>
      </div>
      <div className="settings-section">
        <h2><Globe2 size={18} /> {t("settings.webAccess")}</h2>
        <label className="setting-row"><div><strong>{t("settings.address")}</strong><span>{t("settings.webHint")}</span></div><input value={settings.webHost} onChange={(event) => setSettings({ ...settings, webHost: event.target.value })} /></label>
        <label className="setting-row"><div><strong>{t("settings.port")}</strong></div><input type="number" min={1024} max={65535} value={settings.webPort} onChange={(event) => setSettings({ ...settings, webPort: Number(event.target.value) })} /></label>
        <div className="server-url">{data.serverUrl}</div>
      </div>
      <div className="settings-section">
        <h2><FolderOpen size={18} /> {t("settings.downloads")}</h2>
        <label className="setting-row"><div><strong>{t("settings.downloadLocation")}</strong><span>{t("settings.downloadLocationHint")}</span></div><select value={settings.downloadLocation} onChange={(event) => setSettings({ ...settings, downloadLocation: event.target.value as DownloadLocation })}><option value="source">{t("settings.sourceFolder")}</option><option value="downloads">{t("settings.systemDownloads")}</option><option value="custom">{t("settings.customFolder")}</option></select></label>
        {settings.downloadLocation === "custom" && <div className="setting-row"><div><strong>{t("settings.customFolder")}</strong><span>{t("settings.customFolderHint")}</span></div><div className="setting-path-control"><input value={settings.customDownloadDirectory} onChange={(event) => setSettings({ ...settings, customDownloadDirectory: event.target.value })} placeholder={t("settings.customFolderPlaceholder")} /><button className="icon-text-button compact" type="button" onClick={() => void chooseCustomDirectory()} disabled={!isDesktopApp()} title={t("settings.chooseFolder")}><FolderOpen size={16} /> {t("settings.chooseFolder")}</button></div></div>}
        <label className="setting-row"><div><strong>{t("settings.autoDownload")}</strong><span>{t("settings.autoDownloadHint")}</span></div><span className="setting-toggle"><input type="checkbox" checked={settings.autoDownloadFiles} onChange={(event) => setSettings({ ...settings, autoDownloadFiles: event.target.checked })} aria-label={t("settings.autoDownload")} /></span></label>
        <label className="setting-row"><div><strong>{t("settings.askDownload")}</strong><span>{t("settings.askDownloadHint")}</span></div><span className="setting-toggle"><input type="checkbox" checked={settings.askDownloadLocation} onChange={(event) => setSettings({ ...settings, askDownloadLocation: event.target.checked })} aria-label={t("settings.askDownload")} /></span></label>
      </div>
      <div className="settings-section">
        <h2>{t("settings.ai")}</h2>
        <label className="setting-row"><div><strong>{t("settings.timeout")}</strong><span>{t("settings.timeoutHint")}</span></div><input type="number" min={10} max={3600} step={10} value={settings.aiTimeoutSeconds} onChange={(event) => setSettings({ ...settings, aiTimeoutSeconds: Number(event.target.value) })} /></label>
        <label className="setting-row"><div><strong>{t("settings.logging")}</strong><span>{t("settings.loggingHint")}</span></div><span className="setting-toggle"><input type="checkbox" checked={settings.loggingEnabled} onChange={(event) => setSettings({ ...settings, loggingEnabled: event.target.checked })} aria-label={t("settings.logging")} /></span></label>
        <label className="setting-row"><div><strong>{t("settings.logLevel")}</strong><span>{t("settings.logLevelHint")}</span></div><select value={settings.logLevel} onChange={(event) => setSettings({ ...settings, logLevel: event.target.value as AppSettings["logLevel"] })}><option value="debug">debug</option><option value="info">info</option><option value="warn">warn</option><option value="error">error</option></select></label>
      </div>
      <div className="action-row"><button className="primary-button" onClick={save}><Save size={18} /> {t("common.save")}</button></div>
    </section>
  );
}
