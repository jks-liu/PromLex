import { Bot, CheckCircle2, Pencil, Plus, Search, Trash2, Wifi } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api";
import { Modal } from "../components/Modal";
import type { BootstrapData, Provider, ProviderModel } from "../types";

const DEFAULT_PROVIDER = {
  name: "OpenAI-compatible provider",
  baseUrl: "https://api.openai.com/v1",
  model: "gpt-4.1-mini",
};

function blankProvider(): Provider {
  return {
    id: crypto.randomUUID(),
    ...DEFAULT_PROVIDER,
    reasoningParser: "auto",
    apiKey: "",
    proxyMode: "settings",
    enabled: true,
    supportsImages: false,
    contextSize: 32_768,
    maxSegments: 16,
    maxConcurrent: 2,
    textTranslationModel: false,
  };
}

export function ProvidersPage({ data, onReload }: { data: BootstrapData; onReload: () => Promise<void> }) {
  const { t } = useTranslation();
  const [editing, setEditing] = useState<Provider | null>(null);
  const [models, setModels] = useState<ProviderModel[]>([]);
  const [discovering, setDiscovering] = useState(false);
  const [testing, setTesting] = useState<string | null>(null);
  const [testMessage, setTestMessage] = useState("");
  const [error, setError] = useState("");

  const edit = (provider: Provider) => {
    setModels([]);
    setError("");
    setEditing({ ...provider });
  };

  const discover = async () => {
    if (!editing) return;
    setDiscovering(true);
    setError("");
    try {
      const nextModels = await api.discoverModels(editing.baseUrl, editing.apiKey, editing.proxyMode);
      setModels(nextModels);
      const selected = nextModels.find((model) => model.id === editing.model) || nextModels[0];
      if (selected) {
        setEditing((current) => current ? {
          ...current,
          model: nextModels.some((model) => model.id === current.model) ? current.model : selected.id,
          contextSize: selected.contextSize || current.contextSize,
        } : current);
      }
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setDiscovering(false);
    }
  };

  const save = async () => {
    if (!editing) return;
    setError("");
    try {
      await api.saveProvider(editing);
      setEditing(null);
      await onReload();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const test = async (id: string) => {
    setTesting(id);
    setError("");
    setTestMessage("");
    try {
      const response = await api.testProvider(id);
      setTestMessage(response.message || t("provider.connected"));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setTesting(null);
    }
  };

  return (
    <section className="page">
      <div className="page-heading">
        <div><h1>{t("provider.title")}</h1><p>{t("provider.subtitle")}</p></div>
        <button className="primary-button" onClick={() => { setModels([]); setError(""); setEditing(blankProvider()); }}><Plus size={18} /> {t("provider.new")}</button>
      </div>
      {error && <div className="alert error-alert">{error}</div>}
      {testMessage && <div className="alert success-alert"><CheckCircle2 size={18} /> {testMessage}</div>}
      <div className="item-list">
        {data.providers.map((provider) => (
          <article className="list-item provider-item" key={provider.id}>
            <div className="list-icon dark"><Bot size={19} /></div>
            <div className="list-content"><strong>{provider.name}</strong><span>{provider.model || t("common.noModel")} · {provider.contextSize.toLocaleString()} {t("provider.contextUnit")} · {provider.maxSegments} {t("provider.segmentsUnit")} · {provider.maxConcurrent} {t("provider.concurrentUnit")}</span></div>
            <span className={`badge ${provider.enabled ? "enabled" : ""}`}>{provider.enabled ? t("common.enabled") : t("common.disabled")}</span>
            <div className="list-actions">
              <button className="icon-text-button compact" onClick={() => void test(provider.id)} disabled={testing === provider.id}><Wifi size={16} /> {testing === provider.id ? t("common.testing") : t("common.test")}</button>
              <button className="icon-button" onClick={() => edit(provider)} title={t("common.edit")} aria-label={t("common.edit")}><Pencil size={17} /></button>
              <button className="icon-button danger" onClick={async () => { await api.deleteProvider(provider.id); await onReload(); }} title={t("common.delete")} aria-label={t("common.delete")}><Trash2 size={17} /></button>
            </div>
          </article>
        ))}
      </div>
      {editing && (
        <Modal title={editing.name || t("provider.new")} onClose={() => setEditing(null)} footer={<><button className="secondary-button" onClick={() => setEditing(null)}>{t("common.cancel")}</button><button className="primary-button" onClick={save} disabled={!editing.name || !editing.baseUrl || !editing.model || !editing.contextSize}>{t("common.save")}</button></>}>
          <div className="form-grid two-columns">
            <label className="full"><span>{t("provider.baseUrl")}</span><input value={editing.baseUrl} onChange={(event) => setEditing({ ...editing, baseUrl: event.target.value })} placeholder="http://127.0.0.1:11434/v1" /></label>
            <p className="field-hint full">{t("provider.compatibilityHint")}</p>
            <label><span>{t("common.name")}</span><input value={editing.name} onChange={(event) => setEditing({ ...editing, name: event.target.value })} /></label>
            <label><span>{t("provider.apiKey")}</span><input type="password" value={editing.apiKey} onChange={(event) => setEditing({ ...editing, apiKey: event.target.value })} placeholder={t("provider.apiKeyHint")} /></label>
            <label><span>{t("provider.proxyMode")}</span><select value={editing.proxyMode} onChange={(event) => setEditing({ ...editing, proxyMode: event.target.value as Provider["proxyMode"] })}><option value="none">{t("provider.proxyNone")}</option><option value="settings">{t("provider.proxySettings")}</option><option value="system">{t("provider.proxySystem")}</option></select></label>
            <label className="full"><span>{t("provider.model")}</span><div className="inline-control"><input list="promlex-provider-models" value={editing.model} onChange={(event) => {
              const model = models.find((item) => item.id === event.target.value);
              setEditing({ ...editing, model: event.target.value, contextSize: model?.contextSize || editing.contextSize });
            }} /><button className="icon-text-button compact" type="button" onClick={() => void discover()} disabled={discovering || !editing.baseUrl} title={t("provider.discoverModels")}><Search size={16} /> {discovering ? t("provider.discovering") : t("provider.discoverModels")}</button></div><datalist id="promlex-provider-models">{models.map((model) => <option value={model.id} key={model.id}>{model.contextSize ? `${model.id} (${model.contextSize.toLocaleString()})` : model.id}</option>)}</datalist></label>
            <label className="full"><span>{t("provider.reasoningParser")}</span><select value={editing.reasoningParser} onChange={(event) => setEditing({ ...editing, reasoningParser: event.target.value as Provider["reasoningParser"] })}><option value="auto">{t("provider.reasoningParserAuto")}</option><option value="openai">OpenAI Responses API</option><option value="qwen3">qwen3</option><option value="deepseek_r1">deepseek_r1</option><option value="deepseek_v3">deepseek_v3</option><option value="glm45">glm45</option><option value="gemma4">gemma4</option><option value="granite">granite</option></select><small className="field-hint">{t("provider.reasoningParserHint")}</small></label>
            <label><span>{t("provider.contextSize")}</span><input type="number" min={1024} max={2000000} step={1024} value={editing.contextSize} onChange={(event) => setEditing({ ...editing, contextSize: Number(event.target.value) })} /></label>
            <label><span>{t("provider.maxSegments")}</span><input type="number" min={1} max={1024} value={editing.maxSegments} onChange={(event) => setEditing({ ...editing, maxSegments: Number(event.target.value) })} /></label>
            <label><span>{t("provider.maxConcurrent")}</span><input type="number" min={1} max={64} value={editing.maxConcurrent} onChange={(event) => setEditing({ ...editing, maxConcurrent: Number(event.target.value) })} /></label>
            <label className="switch-row provider-switch-row"><input type="checkbox" checked={editing.textTranslationModel} onChange={(event) => setEditing({ ...editing, textTranslationModel: event.target.checked })} /><div><span>{t("provider.textTranslationModel")}</span><small>{t("provider.textTranslationModelHint")}</small></div></label>
            <label className="switch-row"><input type="checkbox" checked={editing.enabled} onChange={(event) => setEditing({ ...editing, enabled: event.target.checked })} /><span>{t("common.enabled")}</span></label>
            <label className="switch-row"><input type="checkbox" checked={editing.supportsImages} onChange={(event) => setEditing({ ...editing, supportsImages: event.target.checked })} /><span>{t("provider.images")}</span></label>
          </div>
        </Modal>
      )}
    </section>
  );
}
