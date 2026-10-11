import { Download, Pencil, Plus, Sparkles, Trash2, Upload } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api";
import { Modal } from "../components/Modal";
import type { BootstrapData, PromptTemplate } from "../types";
import { downloadJson, nowIso, readJsonFile } from "../utils";

function blankPrompt(): PromptTemplate {
  return { id: crypto.randomUUID(), name: "", content: "", updatedAt: nowIso() };
}

export function PromptsPage({ data, onReload }: { data: BootstrapData; onReload: () => Promise<void> }) {
  const { t } = useTranslation();
  const importRef = useRef<HTMLInputElement>(null);
  const [editing, setEditing] = useState<PromptTemplate | null>(null);
  const [error, setError] = useState("");

  const save = async () => {
    if (!editing?.name.trim() || !editing.content.trim()) return;
    try {
      await api.savePrompt({ ...editing, name: editing.name.trim(), content: editing.content.trim(), updatedAt: nowIso() });
      setEditing(null);
      await onReload();
    } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
  };

  const importData = async (file?: File) => {
    if (!file) return;
    try { await api.importData("prompts", await readJsonFile(file)); await onReload(); }
    catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
  };

  return (
    <section className="page">
      <div className="page-heading">
        <div><h1>{t("prompt.title")}</h1><p>{t("prompt.subtitle")}</p></div>
        <div className="heading-actions">
          <button className="icon-text-button" onClick={() => importRef.current?.click()}><Upload size={17} /> {t("common.import")}</button>
          <button className="icon-text-button" onClick={() => downloadJson("promlex-prompts.json", data.prompts)}><Download size={17} /> {t("common.export")}</button>
          <button className="primary-button" onClick={() => setEditing(blankPrompt())}><Plus size={18} /> {t("prompt.new")}</button>
        </div>
      </div>
      <input ref={importRef} className="visually-hidden" type="file" accept="application/json,.json" onChange={(event) => importData(event.target.files?.[0])} />
      {error && <div className="alert error-alert">{error}</div>}
      <div className="item-list">
        {data.prompts.length === 0 && <div className="empty-state"><Sparkles size={30} /><span>{t("prompt.empty")}</span></div>}
        {data.prompts.map((prompt) => (
          <article className="list-item" key={prompt.id}>
            <div className="list-icon accent"><Sparkles size={19} /></div>
            <div className="list-content"><strong>{prompt.name}</strong><span className="truncate">{prompt.content}</span></div>
            <div className="list-actions">
              <button className="icon-button" onClick={() => setEditing({ ...prompt })}><Pencil size={17} /></button>
              <button className="icon-button danger" onClick={async () => { await api.deletePrompt(prompt.id); await onReload(); }}><Trash2 size={17} /></button>
            </div>
          </article>
        ))}
      </div>
      {editing && (
        <Modal title={editing.name || t("prompt.new")} onClose={() => setEditing(null)} footer={<><button className="secondary-button" onClick={() => setEditing(null)}>{t("common.cancel")}</button><button className="primary-button" onClick={save}>{t("common.save")}</button></>}>
          <div className="form-grid">
            <label><span>{t("common.name")}</span><input value={editing.name} onChange={(event) => setEditing({ ...editing, name: event.target.value })} autoFocus /></label>
            <label><span>{t("prompt.content")}</span><textarea className="prompt-editor" value={editing.content} onChange={(event) => setEditing({ ...editing, content: event.target.value })} /></label>
            <p className="field-hint">{t("prompt.variables")}</p>
          </div>
        </Modal>
      )}
    </section>
  );
}
