import { ArrowLeftRight, ChevronDown } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Glossary, PromptTemplate, Provider, ReasoningEffort } from "../types";

const LANGUAGES = ["en", "zh-CN", "zh-TW", "ja", "ko", "fr", "de", "es", "ru", "ar", "tlh", "火星文"];

export interface TranslationOptionsValue {
  sourceLanguage: string;
  targetLanguage: string;
  providerId: string;
  promptId: string;
  additionalPrompt: string;
  glossaryIds: string[];
  reasoningEffort: ReasoningEffort;
}

interface TranslationOptionsProps {
  value: TranslationOptionsValue;
  onChange: (next: TranslationOptionsValue) => void;
  providers: Provider[];
  prompts: PromptTemplate[];
  glossaries: Glossary[];
}

export function TranslationOptions({ value, onChange, providers, prompts, glossaries }: TranslationOptionsProps) {
  const { t } = useTranslation();
  const update = (patch: Partial<TranslationOptionsValue>) => onChange({ ...value, ...patch });

  const toggleGlossary = (id: string) => {
    const glossaryIds = value.glossaryIds.includes(id)
      ? value.glossaryIds.filter((item) => item !== id)
      : [...value.glossaryIds, id];
    update({ glossaryIds });
  };

  return (
    <div className="translation-options">
      <div className="language-pair">
        <LanguageInput
          label={t("translate.sourceLanguage")}
          value={value.sourceLanguage}
          allowAuto
          onChange={(sourceLanguage) => update({ sourceLanguage })}
        />
        <button
          className="icon-button language-swap"
          type="button"
          onClick={() => update({ sourceLanguage: value.targetLanguage, targetLanguage: value.sourceLanguage })}
          disabled={value.sourceLanguage === "auto"}
          title={t("translate.swapLanguages")}
          aria-label={t("translate.swapLanguages")}
        >
          <ArrowLeftRight size={18} />
        </button>
        <LanguageInput
          label={t("translate.targetLanguage")}
          value={value.targetLanguage}
          onChange={(targetLanguage) => update({ targetLanguage })}
        />
        <small className="language-custom-hint">{t("languages.customHint")}</small>
      </div>
      <label>
        <span>{t("translate.provider")}</span>
        <select value={value.providerId} onChange={(event) => update({ providerId: event.target.value })}>
          {providers.filter((provider) => provider.enabled).map((provider) => <option key={provider.id} value={provider.id}>{provider.name}</option>)}
        </select>
      </label>
      <label>
        <span>{t("translate.prompt")}</span>
        <select value={value.promptId} onChange={(event) => update({ promptId: event.target.value })}>
          <option value="">{t("common.none")}</option>
          {prompts.map((prompt) => <option key={prompt.id} value={prompt.id}>{prompt.name}</option>)}
        </select>
      </label>
      <label className="additional-prompt">
        <span>{t("translate.additionalPrompt")}</span>
        <textarea
          value={value.additionalPrompt}
          onChange={(event) => update({ additionalPrompt: event.target.value })}
          placeholder={t("translate.additionalPromptPlaceholder")}
        />
      </label>
      <label>
        <span>{t("translate.reasoning")}</span>
        <select value={value.reasoningEffort} onChange={(event) => update({ reasoningEffort: event.target.value as ReasoningEffort })}>
          <option value="none">{t("translate.reasoningNone")}</option>
          <option value="low">{t("translate.reasoningLow")}</option>
          <option value="medium">{t("translate.reasoningMedium")}</option>
          <option value="high">{t("translate.reasoningHigh")}</option>
        </select>
      </label>
      <fieldset className="glossary-picker">
        <legend>{t("translate.glossaries")}</legend>
        <div>
          {glossaries.length === 0 && <span className="muted">{t("common.none")}</span>}
          {glossaries.map((glossary) => (
            <label key={glossary.id} className="check-row">
              <input type="checkbox" checked={value.glossaryIds.includes(glossary.id)} onChange={() => toggleGlossary(glossary.id)} />
              <span>{glossary.name}</span>
            </label>
          ))}
        </div>
      </fieldset>
    </div>
  );
}

function LanguageInput({
  label,
  value,
  allowAuto = false,
  onChange,
}: {
  label: string;
  value: string;
  allowAuto?: boolean;
  onChange: (value: string) => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const inputId = useId();
  const listId = useId();
  const languages = allowAuto ? ["auto", ...LANGUAGES] : LANGUAGES;
  const selectedLanguage = value === "zh" ? "zh-CN" : value;
  const displayValue = languages.includes(selectedLanguage)
    ? t(`languages.${selectedLanguage}`)
    : value;

  useEffect(() => {
    if (!open) return;
    const closeOnOutsideClick = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", closeOnOutsideClick);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsideClick);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [open]);

  return (
    <div className="language-input" ref={rootRef}>
      <label htmlFor={inputId}>{label}</label>
      <div className="language-combobox">
        <input
          id={inputId}
          value={displayValue}
          onChange={(event) => {
            onChange(event.target.value);
            setOpen(true);
          }}
          onFocus={() => setOpen(true)}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") setOpen(true);
          }}
          placeholder={t("languages.customPlaceholder")}
          spellCheck={false}
          role="combobox"
          aria-autocomplete="none"
          aria-expanded={open}
          aria-controls={listId}
        />
        <button
          className="language-list-button"
          type="button"
          onClick={() => setOpen((current) => !current)}
          aria-label={t("languages.showOptions")}
          aria-expanded={open}
          aria-controls={listId}
          tabIndex={-1}
        >
          <ChevronDown size={16} />
        </button>
        {open && (
          <div className="language-options" id={listId} role="listbox">
            {languages.map((language) => (
              <button
                type="button"
                role="option"
                aria-selected={selectedLanguage === language}
                className={selectedLanguage === language ? "selected" : ""}
                key={language}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => {
                  onChange(language);
                  setOpen(false);
                }}
              >
                <span>{t(`languages.${language}`)}</span>
                <small>{language}</small>
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
