import { ExternalLink, RefreshCw } from "lucide-react";
import { useCallback, useEffect, useState, type MouseEvent } from "react";
import { useTranslation } from "react-i18next";
import { api, isDesktopApp } from "../api";
import type { ReleaseCheck } from "../types";

const projectUrl = "https://github.com/jks-liu/PromLex";

export function AboutPage({ version }: { version: string }) {
  const { t } = useTranslation();
  const [checking, setChecking] = useState(true);
  const [release, setRelease] = useState<ReleaseCheck | null>(null);
  const [failed, setFailed] = useState(false);
  const [linkError, setLinkError] = useState("");
  const check = useCallback(async (signal?: AbortSignal) => {
    setChecking(true);
    setFailed(false);
    try {
      const latest = await api.checkRelease(signal);
      if (!signal?.aborted) setRelease(latest);
    } catch {
      if (!signal?.aborted) setFailed(true);
    } finally {
      if (!signal?.aborted) setChecking(false);
    }
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    void check(controller.signal);
    return () => controller.abort();
  }, [check]);

  const openLink = (event: MouseEvent<HTMLAnchorElement>, releases: boolean) => {
    if (!isDesktopApp()) return;
    event.preventDefault();
    setLinkError("");
    void api.openProjectPage(releases).catch((reason) => setLinkError(String(reason)));
  };

  const updateStatus = checking ? t("about.checking")
    : failed ? t("about.checkFailed")
    : release?.updateAvailable ? t("about.newVersion", { version: release.latestVersion })
    : release?.latestVersion ? t("about.upToDate")
    : t("about.noRelease");

  return (
    <section className="page about-page" aria-labelledby="about-heading">
      <div className="page-heading">
        <div><h1 id="about-heading">{t("nav.about")}</h1><p>{t("common.productName")} · v{version}</p></div>
      </div>
      <div className="settings-section">
        <h2>{t("about.updates")}</h2>
        <div className="about-content">
          <p role="status">{updateStatus}</p>
          <div className="heading-actions">
            <button className="secondary-button" onClick={() => void check()} disabled={checking}><RefreshCw size={16} /> {t("about.check")}</button>
            <a className="icon-text-button" href={`${projectUrl}/releases`} target="_blank" rel="noreferrer" onClick={(event) => openLink(event, true)}><ExternalLink size={16} /> {t("about.releases")}</a>
          </div>
        </div>
      </div>
      <div className="about-links">
        <a className="icon-text-button" href={projectUrl} target="_blank" rel="noreferrer" onClick={(event) => openLink(event, false)}><ExternalLink size={16} /> {t("about.project")}</a>
        <small>Copyright © LIU Jinchen</small>
      </div>
      {linkError && <p className="transfer-error" role="alert">{linkError}</p>}
    </section>
  );
}
