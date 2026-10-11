import { Check, Clipboard, Languages } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { translateStream } from "../api";
import { TranslationOptions, type TranslationOptionsValue } from "../components/TranslationOptions";
import type { BootstrapData } from "../types";

export function TranslatePage({ data, onReload }: { data: BootstrapData; onReload: () => Promise<void> }) {
  const { t } = useTranslation();
  const defaultProvider = useMemo(() => data.providers.find((provider) => provider.enabled)?.id || "", [data.providers]);
  const [options, setOptions] = useState<TranslationOptionsValue>(() => readOptions(defaultProvider, data.prompts[0]?.id || ""));
  const [source, setSource] = useState("");
  const [result, setResult] = useState("");
  const [working, setWorking] = useState(false);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    setOptions((current) => {
      const providerExists = data.providers.some((provider) => provider.enabled && provider.id === current.providerId);
      const promptExists = !current.promptId || data.prompts.some((prompt) => prompt.id === current.promptId);
      const next = {
        ...current,
        providerId: providerExists ? current.providerId : defaultProvider,
        promptId: promptExists ? current.promptId : data.prompts[0]?.id || "",
      };
      return JSON.stringify(next) === JSON.stringify(current) ? current : next;
    });
  }, [data.providers, data.prompts, defaultProvider]);

  useEffect(() => {
    localStorage.setItem("tranova-translate-options", JSON.stringify(options));
  }, [options]);

  const translate = async () => {
    if (!source.trim() || !options.providerId) return;
    setWorking(true);
    setError("");
    try {
      setResult("");
      const response = await translateStream(
        { text: source, ...options, promptId: options.promptId || undefined },
        (text) => setResult(text),
      );
      setResult(response.translatedText);
      await onReload();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setWorking(false);
    }
  };

  const copy = async () => {
    await navigator.clipboard.writeText(result);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };

  const changeOptions = (next: TranslationOptionsValue) => {
    setOptions(next);
    localStorage.setItem("tranova-translate-options", JSON.stringify(next));
  };

  return (
    <section className="page translate-page">
      <div className="page-heading">
        <div><h1>{t("translate.title")}</h1><p>{t("translate.subtitle")}</p></div>
      </div>
      <TranslationOptions value={options} onChange={changeOptions} providers={data.providers} prompts={data.prompts} glossaries={data.glossaries} />
      <div className="editor-grid">
        <section className="editor-panel">
          <header><strong>{t("translate.source")}</strong><span>{t("translate.characters", { count: source.length })}</span></header>
          <textarea value={source} onChange={(event) => setSource(event.target.value)} placeholder={t("translate.placeholder")} autoFocus />
        </section>
        <section className="editor-panel result-panel">
          <header>
            <strong>{t("translate.target")}</strong>
            <button className="icon-button" onClick={copy} disabled={!result} title={t("translate.copy")} aria-label={t("translate.copy")}>
              {copied ? <Check size={18} /> : <Clipboard size={18} />}
            </button>
          </header>
          <div className={`translation-result ${!result ? "empty" : ""}`}>{result || t("translate.target")}</div>
        </section>
      </div>
      {error && <div className="alert error-alert">{error}</div>}
      <div className="action-row">
        <button className="primary-button" onClick={translate} disabled={working || !source.trim() || !options.providerId}>
          <Languages size={18} /> {working ? t("translate.working") : t("translate.action")}
        </button>
      </div>
    </section>
  );
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
    const stored = JSON.parse(localStorage.getItem("tranova-translate-options") || "null") as Partial<TranslationOptionsValue> | null;
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
