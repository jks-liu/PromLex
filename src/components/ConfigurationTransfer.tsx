import { Download, Upload } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api";
import type { BootstrapData } from "../types";
import { downloadJson, readJsonFile } from "../utils";
import { Modal } from "./Modal";

export function ConfigurationTransfer({ data, scope, onImported }: {
  data: BootstrapData;
  scope: "configuration" | "providers";
  onImported: (data: BootstrapData) => Promise<void>;
}) {
  const { t } = useTranslation();
  const input = useRef<HTMLInputElement>(null);
  const [showExport, setShowExport] = useState(false);
  const [includeKeys, setIncludeKeys] = useState(false);
  const [importing, setImporting] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");

  const readBackup = async (file: File) => {
    setImporting(true);
    setMessage("");
    setError("");
    try {
      const imported = await api.importData(scope, await readJsonFile(file));
      await onImported(imported);
      setMessage(t("transfer.imported"));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setImporting(false);
    }
  };

  const exportBackup = () => {
    const providers = data.providers.map((provider) => ({ ...provider, apiKey: includeKeys ? provider.apiKey : undefined }));
    const backup = scope === "configuration"
      ? { settings: data.settings, providers, glossaries: data.glossaries, prompts: data.prompts }
      : { providers };
    downloadJson(`promlex-${scope}.json`, backup);
    setShowExport(false);
  };

  return (
    <div className="configuration-transfer">
      <div className="heading-actions">
        <button className="icon-text-button" disabled={importing} onClick={() => input.current?.click()}><Upload size={17} /> {t(`transfer.${scope}Import`)}</button>
        <button className="icon-text-button" disabled={importing} onClick={() => { setIncludeKeys(false); setShowExport(true); }}><Download size={17} /> {t(`transfer.${scope}Export`)}</button>
      </div>
      <input ref={input} className="visually-hidden" type="file" accept="application/json,.json" aria-label={t(`transfer.${scope}Import`)} onChange={(event) => {
        const file = event.target.files?.[0];
        event.target.value = "";
        if (file) void readBackup(file);
      }} />
      {message && <p className="transfer-message" role="status">{message}</p>}
      {error && <p className="transfer-message transfer-error" role="alert">{error}</p>}
      {showExport && (
        <Modal title={t(`transfer.${scope}Export`)} onClose={() => setShowExport(false)} footer={<>
          <button className="secondary-button" onClick={() => setShowExport(false)}>{t("common.cancel")}</button>
          <button className="primary-button" onClick={exportBackup}>{t("common.export")}</button>
        </>}>
          <label className="check-row">
            <input type="checkbox" checked={includeKeys} onChange={(event) => setIncludeKeys(event.target.checked)} />
            <span>{t("transfer.includeKeys")}</span>
          </label>
        </Modal>
      )}
    </div>
  );
}
