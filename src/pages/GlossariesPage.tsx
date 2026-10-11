import { Download, Library, Pencil, Plus, Trash2, Upload, X } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api";
import { Modal } from "../components/Modal";
import type { BootstrapData, Glossary } from "../types";
import { downloadJson, nowIso, readJsonFile } from "../utils";

function blankGlossary(): Glossary {
  return { id: crypto.randomUUID(), name: "", sourceLanguage: "auto", targetLanguage: "zh-CN", entries: [], updatedAt: nowIso() };
}

export function GlossariesPage({ data, onReload }: { data: BootstrapData; onReload: () => Promise<void> }) {
  const { t } = useTranslation();
  const importRef = useRef<HTMLInputElement>(null);
  const [editing, setEditing] = useState<Glossary | null>(null);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);

  const save = async () => {
    if (!editing?.name.trim()) return;
    setSaving(true);
    setError("");
    try {
      await api.saveGlossary({ ...editing, name: editing.name.trim(), updatedAt: nowIso(), entries: editing.entries.filter((entry) => entry.source.trim() && entry.target.trim()) });
      setEditing(null);
      await onReload();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally { setSaving(false); }
  };

  const remove = async (id: string) => {
    await api.deleteGlossary(id);
    await onReload();
  };

  const importData = async (file?: File) => {
    if (!file) return;
    try {
      await api.importData("glossaries", await readJsonFile(file));
      await onReload();
    } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
  };

  return (
    <section className="page">
      <div className="page-heading">
        <div><h1>{t("glossary.title")}</h1><p>{t("glossary.subtitle")}</p></div>
        <div className="heading-actions">
          <button className="icon-text-button" onClick={() => importRef.current?.click()}><Upload size={17} /> {t("common.import")}</button>
          <button className="icon-text-button" onClick={() => downloadJson("promlex-glossaries.json", data.glossaries)}><Download size={17} /> {t("common.export")}</button>
          <button className="primary-button" onClick={() => setEditing(blankGlossary())}><Plus size={18} /> {t("glossary.new")}</button>
        </div>
      </div>
      <input ref={importRef} className="visually-hidden" type="file" accept="application/json,.json" onChange={(event) => importData(event.target.files?.[0])} />
      {error && <div className="alert error-alert">{error}</div>}
      <div className="item-list">
        {data.glossaries.length === 0 && <div className="empty-state"><Library size={30} /><span>{t("glossary.empty")}</span></div>}
        {data.glossaries.map((glossary) => (
          <article className="list-item" key={glossary.id}>
            <div className="list-icon"><Library size={19} /></div>
            <div className="list-content"><strong>{glossary.name}</strong><span>{t("glossary.entries", { count: glossary.entries.length })} · {glossary.sourceLanguage} → {glossary.targetLanguage}</span></div>
            <div className="list-actions">
              <button className="icon-button" onClick={() => setEditing(structuredClone(glossary))} title={t("common.edit")}><Pencil size={17} /></button>
              <button className="icon-button danger" onClick={() => remove(glossary.id)} title={t("common.delete")}><Trash2 size={17} /></button>
            </div>
          </article>
        ))}
      </div>
      {editing && (
        <Modal title={editing.name || t("glossary.new")} onClose={() => setEditing(null)} wide footer={<><button className="secondary-button" onClick={() => setEditing(null)}>{t("common.cancel")}</button><button className="primary-button" onClick={save} disabled={saving || !editing.name.trim()}>{t("common.save")}</button></>}>
          <div className="form-grid two-columns">
            <label className="full"><span>{t("common.name")}</span><input value={editing.name} onChange={(event) => setEditing({ ...editing, name: event.target.value })} autoFocus /></label>
            <label><span>{t("glossary.sourceLanguage")}</span><input value={editing.sourceLanguage} onChange={(event) => setEditing({ ...editing, sourceLanguage: event.target.value })} /></label>
            <label><span>{t("glossary.targetLanguage")}</span><input value={editing.targetLanguage} onChange={(event) => setEditing({ ...editing, targetLanguage: event.target.value })} /></label>
          </div>
          <div className="terms-table">
            <div className="terms-head"><span>{t("glossary.sourceTerm")}</span><span>{t("glossary.targetTerm")}</span><span>{t("glossary.note")}</span><span /></div>
            {editing.entries.map((entry, index) => (
              <div className="term-row" key={index}>
                <input value={entry.source} onChange={(event) => { const entries = [...editing.entries]; entries[index] = { ...entry, source: event.target.value }; setEditing({ ...editing, entries }); }} />
                <input value={entry.target} onChange={(event) => { const entries = [...editing.entries]; entries[index] = { ...entry, target: event.target.value }; setEditing({ ...editing, entries }); }} />
                <input value={entry.note || ""} onChange={(event) => { const entries = [...editing.entries]; entries[index] = { ...entry, note: event.target.value }; setEditing({ ...editing, entries }); }} />
                <button className="icon-button" onClick={() => setEditing({ ...editing, entries: editing.entries.filter((_, itemIndex) => itemIndex !== index) })}><X size={16} /></button>
              </div>
            ))}
          </div>
          <button className="text-button" onClick={() => setEditing({ ...editing, entries: [...editing.entries, { source: "", target: "", note: "" }] })}><Plus size={17} /> {t("glossary.addTerm")}</button>
        </Modal>
      )}
    </section>
  );
}
