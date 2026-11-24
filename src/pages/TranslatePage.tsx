import { Ban, Check, Clipboard, Languages, Play } from "lucide-react";
import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { translateStream } from "../api";
import { languageKey, TranslationOptions, type TranslationOptionsValue } from "../components/TranslationOptions";
import { TranslationTabs, useTranslationTabs } from "../components/TranslationTabs";
import type { BootstrapData } from "../types";

interface PageProps {
  data: BootstrapData;
  onReload: () => Promise<void>;
}

const MarkdownText = lazy(() => import("../components/MarkdownText"));

export function TranslatePage({ data, onReload }: PageProps) {
  const { t } = useTranslation();
  const session = useTranslationTabs(data, "text");
  const [renderMarkdown, setRenderMarkdown] = useState(() => localStorage.getItem("tranova-render-markdown") === "true");

  const changeMarkdown = (enabled: boolean) => {
    setRenderMarkdown(enabled);
    localStorage.setItem("tranova-render-markdown", String(enabled));
  };

  return (
    <section className="page translate-page" aria-label={t("translate.title")}>
      <TranslationTabs session={session} />
      {session.tabs.map((tab, index) => (
        <div key={tab.id} id={`${tab.id}-panel`} role="tabpanel" aria-labelledby={tab.id} hidden={tab.id !== session.active.id}>
          <TextTranslation
            data={data}
            onReload={onReload}
            options={tab.options}
            onOptionsChange={(options) => session.changeOptions(tab.id, options)}
            renderMarkdown={renderMarkdown}
            onMarkdownChange={changeMarkdown}
            autoFocus={index === 0}
          />
        </div>
      ))}
    </section>
  );
}

function TextTranslation({
  data, onReload, options, onOptionsChange, renderMarkdown, onMarkdownChange, autoFocus,
}: PageProps & {
  options: TranslationOptionsValue;
  onOptionsChange: (options: TranslationOptionsValue) => void;
  renderMarkdown: boolean;
  onMarkdownChange: (enabled: boolean) => void;
  autoFocus: boolean;
}) {
  const { t } = useTranslation();
  const [source, setSource] = useState("");
  const [result, setResult] = useState("");
  const [working, setWorking] = useState(false);
  const [cancelled, setCancelled] = useState(false);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState(false);
  const request = useRef<AbortController>(null);
  const copyTimer = useRef<number>(undefined);
  const target = languageKey(options.targetLanguage);
  const secondary = languageKey(options.secondaryTargetLanguage || "");
  const sameLanguage = Boolean(target && secondary === target);
  const canTranslate = Boolean(source.trim() && options.providerId && target && secondary && !sameLanguage);

  useEffect(() => () => {
    request.current?.abort();
    window.clearTimeout(copyTimer.current);
  }, []);

  const translate = async (continueTranslation: boolean) => {
    if (!canTranslate || request.current) return;
    const controller = new AbortController();
    request.current = controller;
    setWorking(true);
    setCancelled(false);
    setError("");
    if (!continueTranslation) setResult("");
    try {
      const response = await translateStream(
        {
          ...options,
          text: source,
          sourceLanguage: "auto",
          promptId: options.promptId || undefined,
          existingTranslation: continueTranslation ? result : undefined,
        },
        (text) => {
          if (!controller.signal.aborted) setResult(text);
        },
        controller.signal,
      );
      if (!controller.signal.aborted) {
        setResult(response.translatedText);
        void onReload();
      }
    } catch (reason) {
      if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (request.current === controller) {
        request.current = null;
        setWorking(false);
      }
    }
  };

  const cancel = () => {
    request.current?.abort();
    request.current = null;
    setWorking(false);
    setCancelled(true);
  };

  const copy = async () => {
    await navigator.clipboard.writeText(result);
    setCopied(true);
    window.clearTimeout(copyTimer.current);
    copyTimer.current = window.setTimeout(() => setCopied(false), 1500);
  };

  return (
    <>
      <TranslationOptions value={options} onChange={onOptionsChange} providers={data.providers} prompts={data.prompts} glossaries={data.glossaries} showSecondaryLanguage />
      {sameLanguage && <p className="alert error-alert">{t("translate.differentLanguages")}</p>}
      <div className="editor-grid">
        <section className="editor-panel">
          <header className="source-header">
            <strong>{t("translate.source")}</strong>
            {working ? (
              <button className="secondary-button source-translate-button" onClick={cancel}>
                <Ban size={16} /> {t("files.cancel")}
              </button>
            ) : (
              <button className="primary-button source-translate-button" onClick={() => void translate(false)} disabled={!canTranslate}>
                <Languages size={16} /> {t("translate.action")}
              </button>
            )}
            <span className="source-character-count">{t("translate.characters", { count: source.length })}</span>
          </header>
          <textarea aria-label={t("translate.source")} value={source} onChange={(event) => setSource(event.target.value)} placeholder={t("translate.placeholder")} readOnly={working} autoFocus={autoFocus} />
        </section>
        <section className="editor-panel result-panel">
          <header>
            <strong>{t("translate.target")}</strong>
            <button className="secondary-button source-translate-button" onClick={() => void translate(true)} disabled={working || !canTranslate || !result.trim()}>
              <Play size={15} /> {t("translate.continue")}
            </button>
            <label className="check-row markdown-toggle"><input type="checkbox" checked={renderMarkdown} onChange={(event) => onMarkdownChange(event.target.checked)} /><span>{t("translate.renderMarkdown")}</span></label>
            <button className="icon-button" onClick={() => void copy()} disabled={!result} title={t("translate.copy")} aria-label={t("translate.copy")}>
              {copied ? <Check size={18} /> : <Clipboard size={18} />}
            </button>
          </header>
          {renderMarkdown && result ? (
            <div className="translation-result markdown-body"><Suspense fallback={result}><MarkdownText text={result} /></Suspense></div>
          ) : (
            <textarea className="translation-result" aria-label={t("translate.target")} placeholder={t("translate.target")} value={result} onChange={(event) => setResult(event.target.value)} readOnly={working} />
          )}
        </section>
      </div>
      {(working || cancelled) && <p className="translation-status muted" role="status">{t(working ? "translate.working" : "translate.cancelled")}</p>}
      {error && <div className="alert error-alert" role="alert">{error}</div>}
    </>
  );
}
