import { Pencil, Plus, X } from "lucide-react";
import { useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { BootstrapData } from "../types";
import { Modal } from "./Modal";
import { languageKey, languageLabel, type TranslationOptionsValue } from "./TranslationOptions";

export function useTranslationTabs(data: BootstrapData, kind: "text" | "file") {
  const prefix = useId();
  const nextId = useRef(1);
  const storageKey = kind === "text" ? "tranova-translate-options" : "tranova-file-options";
  const [activeId, setActive] = useState(`${prefix}-0`);
  const [drafts, setDrafts] = useState(() => [{
    id: `${prefix}-0`,
    name: "",
    options: readOptions(storageKey, data, kind === "text"),
  }]);
  const providerId = data.providers.find((provider) => provider.enabled)?.id || "";
  const tabs = drafts.map((tab) => ({
    ...tab,
    options: {
      ...tab.options,
      providerId: data.providers.some((provider) => provider.enabled && provider.id === tab.options.providerId) ? tab.options.providerId : providerId,
      promptId: !tab.options.promptId || data.prompts.some((prompt) => prompt.id === tab.options.promptId) ? tab.options.promptId : data.prompts[0]?.id || "",
    },
  }));
  const active = tabs.find((tab) => tab.id === activeId) || tabs[0];

  return {
    tabs,
    active,
    setActive,
    add() {
      const id = `${prefix}-${nextId.current++}`;
      setDrafts((current) => [...current, { id, name: "", options: active.options }]);
      setActive(id);
      return id;
    },
    close() {
      if (tabs.length < 2) return active.id;
      const index = tabs.findIndex((tab) => tab.id === active.id);
      const next = (tabs[index + 1] || tabs[index - 1]).id;
      setActive(next);
      setDrafts((current) => current.filter((tab) => tab.id !== active.id));
      return next;
    },
    rename(name: string) {
      setDrafts((current) => current.map((tab) => tab.id === active.id ? { ...tab, name: name.trim() } : tab));
    },
    changeOptions(id: string, options: TranslationOptionsValue) {
      setDrafts((current) => current.map((tab) => tab.id === id ? { ...tab, options } : tab));
      localStorage.setItem(storageKey, JSON.stringify(options));
    },
  };
}

export function TranslationTabs({ session }: { session: ReturnType<typeof useTranslationTabs> }) {
  const { t } = useTranslation();
  const { tabs, active } = session;
  const [name, setName] = useState<string | null>(null);
  const focusTab = (id: string) => window.requestAnimationFrame(() => document.getElementById(id)?.focus());
  const closeRename = () => {
    setName(null);
    focusTab(active.id);
  };
  const saveName = () => {
    if (name !== null) session.rename(name);
    closeRename();
  };

  return (
    <div className="translation-tab-bar">
      <div className="translation-tabs" role="tablist" aria-label={t("tabs.title")}>
        {tabs.map((tab, index) => (
          <button
            type="button"
            role="tab"
            id={tab.id}
            key={tab.id}
            aria-controls={`${tab.id}-panel`}
            aria-selected={tab.id === active.id}
            tabIndex={tab.id === active.id ? 0 : -1}
            onClick={() => session.setActive(tab.id)}
            onKeyDown={(event) => {
              const targetIndex = { ArrowLeft: (index + tabs.length - 1) % tabs.length, ArrowRight: (index + 1) % tabs.length, Home: 0, End: tabs.length - 1 }[event.key];
              if (targetIndex === undefined) return;
              event.preventDefault();
              const id = tabs[targetIndex].id;
              session.setActive(id);
              focusTab(id);
            }}
          >
            {tab.name || languageLabel(tab.options.targetLanguage, t).trim() || t("tabs.untitled")}
          </button>
        ))}
      </div>
      <div className="translation-tab-actions">
        <button type="button" className="icon-button" title={t("tabs.add")} aria-label={t("tabs.add")} onClick={() => focusTab(session.add())}><Plus size={17} /></button>
        <button type="button" className="icon-button" title={t("tabs.rename")} aria-label={t("tabs.rename")} onClick={() => setName(active.name)}><Pencil size={16} /></button>
        <button type="button" className="icon-button" title={t("tabs.close")} aria-label={t("tabs.close")} disabled={tabs.length === 1} onClick={() => focusTab(session.close())}><X size={17} /></button>
      </div>
      {name !== null && (
        <Modal title={t("tabs.rename")} onClose={closeRename} footer={<>
          <button className="secondary-button" onClick={closeRename}>{t("common.cancel")}</button>
          <button className="primary-button" onClick={saveName}>{t("common.save")}</button>
        </>}>
          <label>
            <span>{t("tabs.name")}</span>
            <input autoFocus value={name} placeholder={languageLabel(active.options.targetLanguage, t)} onChange={(event) => setName(event.target.value)} onKeyDown={(event) => {
              if (event.key === "Enter") saveName();
              if (event.key === "Escape") closeRename();
            }} />
          </label>
        </Modal>
      )}
    </div>
  );
}

function readOptions(key: string, data: BootstrapData, showSecondaryLanguage: boolean): TranslationOptionsValue {
  const fallback: TranslationOptionsValue = {
    sourceLanguage: "auto",
    targetLanguage: "zh-CN",
    providerId: data.providers.find((provider) => provider.enabled)?.id || "",
    promptId: data.prompts[0]?.id || "",
    additionalPrompt: "",
    glossaryIds: [],
    reasoningEffort: "none",
  };
  let stored: Partial<TranslationOptionsValue> | null = null;
  try {
    stored = JSON.parse(localStorage.getItem(key) || "null");
  } catch {
    // The saved preferences are optional.
  }
  const targetLanguage = stored?.targetLanguage === "zh" ? "zh-CN" : stored?.targetLanguage || fallback.targetLanguage;
  const secondary = stored?.secondaryTargetLanguage || (languageKey(targetLanguage) === "en" ? "zh-CN" : "en");
  const { reasoningEffort, glossaryIds, providerId, promptId, additionalPrompt } = stored || {};
  return {
    ...fallback,
    targetLanguage,
    ...(showSecondaryLanguage ? { secondaryTargetLanguage: secondary } : {}),
    providerId: providerId ?? fallback.providerId,
    promptId: promptId ?? fallback.promptId,
    additionalPrompt: additionalPrompt ?? fallback.additionalPrompt,
    glossaryIds: Array.isArray(glossaryIds) ? glossaryIds : [],
    reasoningEffort: reasoningEffort === "low" || reasoningEffort === "medium" || reasoningEffort === "high" ? reasoningEffort : "none",
  };
}
