import { Check, Clipboard, Languages } from "lucide-react";
import { lazy, Suspense, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { translateStream } from "../api";
import { TranslationOptions, type TranslationOptionsValue } from "../components/TranslationOptions";
import type { BootstrapData } from "../types";

const MarkdownText = lazy(() => import("../components/MarkdownText"));

export function TranslatePage({ data, onReload }: { data: BootstrapData; onReload: () => Promise<void> }) {
  const { t } = useTranslation();
  const defaultProvider = useMemo(() => data.providers.find((provider) => provider.enabled)?.id || "", [data.providers]);
  const [options, setOptions] = useState<TranslationOptionsValue>(() => readOptions(defaultProvider, data.prompts[0]?.id || ""));
  const [source, setSource] = useState("");
  const [result, setResult] = useState("");
  const [working, setWorking] = useState(false);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const [renderMarkdown, setRenderMarkdown] = useState(() => localStorage.getItem("tranova-render-markdown") === "true");

  useEffect(() => {
    localStorage.setItem("tranova-render-markdown", String(renderMarkdown));
  }, [renderMarkdown]);

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
        { text: source, ...options, sourceLanguage: "auto", promptId: options.promptId || undefined },
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
    <section className="page translate-page" aria-label={t("translate.title")}>
      <TranslationOptions value={options} onChange={changeOptions} providers={data.providers} prompts={data.prompts} glossaries={data.glossaries} />
      <div className="editor-grid">
        <section className="editor-panel">
          <header className="source-header">
            <strong>{t("translate.source")}</strong>
            <button className="primary-button source-translate-button" onClick={translate} disabled={working || !source.trim() || !options.providerId}>
              <Languages size={16} /> {working ? t("translate.working") : t("translate.action")}
            </button>
            <span className="source-character-count">{t("translate.characters", { count: source.length })}</span>
          </header>
          <textarea value={source} onChange={(event) => setSource(event.target.value)} placeholder={t("translate.placeholder")} autoFocus />
        </section>
        <section className="editor-panel result-panel">
          <header>
            <strong>{t("translate.target")}</strong>
            <label className="check-row markdown-toggle"><input type="checkbox" checked={renderMarkdown} onChange={(event) => setRenderMarkdown(event.target.checked)} /><span>{t("translate.renderMarkdown")}</span></label>
            <button className="icon-button" onClick={copy} disabled={!result} title={t("translate.copy")} aria-label={t("translate.copy")}>
              {copied ? <Check size={18} /> : <Clipboard size={18} />}
            </button>
          </header>
          <div className={`translation-result ${!result ? "empty" : ""} ${renderMarkdown && result ? "markdown-body" : ""}`}>
            {result ? renderMarkdown ? <Suspense fallback={result}><MarkdownText text={result} /></Suspense> : result : t("translate.target")}
          </div>
        </section>
      </div>
      {error && <div className="alert error-alert">{error}</div>}
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
    const targetLanguage = stored.targetLanguage === "zh" ? "zh-CN" : stored.targetLanguage;
    return {
      ...fallback,
      ...stored,
      sourceLanguage: "auto",
      ...(targetLanguage ? { targetLanguage } : {}),
      glossaryIds: Array.isArray(stored.glossaryIds) ? stored.glossaryIds : [],
      reasoningEffort: stored.reasoningEffort === "none" || stored.reasoningEffort === "low" || stored.reasoningEffort === "medium" || stored.reasoningEffort === "high" ? stored.reasoningEffort : "none",
    };
  } catch {
    return fallback;
  }
}
