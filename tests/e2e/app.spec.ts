import { expect, test } from "@playwright/test";

for (const viewport of [
  { name: "desktop", width: 1440, height: 900 },
  { name: "mobile", width: 390, height: 844 },
]) {
  test(`${viewport.name} application shell renders without overflow`, async ({ page }) => {
    const browserErrors: string[] = [];
    page.on("pageerror", (error) => browserErrors.push(error.message));
    await page.setViewportSize(viewport);
    await page.goto("/", { waitUntil: "domcontentloaded" });

    await expect(page.locator("h1:visible")).toHaveCount(1);
    await expect(page.locator(".app-shell")).toBeVisible();
    await expect(page).toHaveTitle("PromLex");
    await expect(page.locator(".brand-row strong")).toHaveText("PromLex");
    await expect.poll(() => page.locator(".brand-mark").evaluate((image: HTMLImageElement) => image.complete && image.naturalWidth > 0)).toBe(true);
    await expect(page).toHaveScreenshot(`${viewport.name}.png`, { animations: "disabled", fullPage: false });
    expect(browserErrors).toEqual([]);

    const dimensions = await page.evaluate(() => ({
      viewport: document.documentElement.clientWidth,
      content: document.documentElement.scrollWidth,
      heading: document.querySelector("h1:not([hidden])")?.textContent,
      shell: document.querySelector(".app-shell")?.getBoundingClientRect().toJSON(),
    }));
    expect(dimensions.content).toBeLessThanOrEqual(dimensions.viewport);
    expect(dimensions.heading).toBeTruthy();
    expect(dimensions.shell?.width).toBeGreaterThanOrEqual(viewport.width - 1);
  });
}

test("Simplified Chinese interface localizes the primary workflow", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("tranova-language", "zh-CN"));
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(page.locator("h1:visible")).toHaveText("文本翻译");
  await expect(page.locator(".sidebar-nav")).toContainText("文件翻译");
  await expect(page.locator(".brand-row")).toContainText("AI 翻译工作台");
  await expect(page.locator(".brand-row strong")).toHaveText("语锚 · PromLex");
  await expect(page).toHaveTitle("语锚 · PromLex");
});

test("language options use native names and stay visible while typing", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("tranova-language", "en"));
  await page.goto("/", { waitUntil: "domcontentloaded" });

  const sourceLanguage = page.getByRole("combobox", { name: "Source language" });
  const targetLanguage = page.getByRole("combobox", { name: "Target language" });
  await expect(targetLanguage).toHaveValue("简体中文");
  await targetLanguage.click();
  await targetLanguage.locator("..").getByRole("option", { name: "繁體中文 zh-TW" }).click();
  await expect(targetLanguage).toHaveValue("繁體中文");

  await sourceLanguage.fill("custom language");
  const options = sourceLanguage.locator("..").getByRole("option");
  await expect(options).toHaveCount(13);
  await expect(options).toContainText(["Auto detect", "English", "简体中文", "繁體中文", "日本語", "한국어", "Français", "Deutsch", "Español", "Русский", "العربية", "tlhIngan Hol", "吙煋呅"]);
  await expect(page.locator(".translate-page .language-custom-hint")).toHaveText("You can also enter any language directly.");
});

test("provider enabled checkbox has a visible label", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("tranova-language", "en"));
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "AI providers" }).click();
  await page.getByRole("button", { name: "Add provider" }).click();

  const enabled = page.getByRole("checkbox", { name: "Enabled", exact: true });
  await expect(page.getByText("Enabled", { exact: true })).toBeVisible();
  await expect(enabled).toBeChecked();
});


test("translation action sits above the source editor", async ({ page }) => {
  await page.goto("/", { waitUntil: "domcontentloaded" });
  const button = page.locator(".translate-page .action-row button");
  const editor = page.locator(".translate-page .editor-panel textarea");
  await expect(button).toBeVisible();
  const actionBox = await button.boundingBox();
  const editorBox = await editor.boundingBox();
  expect(actionBox!.y + actionBox!.height).toBeLessThanOrEqual(editorBox!.y);
});

test("browser save as allows renaming the translated download", async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem("tranova-language", "en");
    Object.defineProperty(window, "showSaveFilePicker", { value: undefined, configurable: true });
  });
  await page.route("**/api/file-jobs", (route) => route.fulfill({ json: [{
    id: "save-as", filename: "source.txt", resultFilename: "source-translated.txt",
    state: "completed", stage: "completed", outputMode: "translated", failedBatches: [],
    totalSegments: 1, translatedSegments: 1, totalBatches: 1, completedBatches: 1,
    failedSegments: 0, skippedSegments: 0,
  }] }));
  await page.route("**/api/file-jobs/save-as/download", (route) => route.fulfill({ body: "Translated content", contentType: "text/plain" }));
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Files", exact: true }).click();
  page.once("dialog", (dialog) => dialog.accept("renamed.txt"));
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Save as…", exact: true }).click();
  expect((await download).suggestedFilename()).toBe("renamed.txt");
});
