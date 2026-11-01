import { expect, test } from "@playwright/test";
import { createServer } from "node:http";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

test("folder controls remain usable on narrow screens", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("tranova-language", "en"));
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await page.getByRole("button", { name: "Open menu" }).click();
  await page.getByRole("button", { name: "Files", exact: true }).click();
  await expect(page.getByRole("button", { name: "Choose folder", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Choose output folder", exact: true })).toBeVisible();
  const dimensions = await page.evaluate(() => ({ content: document.documentElement.scrollWidth, viewport: document.documentElement.clientWidth }));
  expect(dimensions.content).toBeLessThanOrEqual(dimensions.viewport);
});

test("Markdown rendering is optional, safe and remembered", async ({ page }) => {
  const markdown = "# Heading\n\n**Bold**\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n<script>window.injected = true</script>\n\n[bad](javascript:alert(1))";
  await page.addInitScript(() => {
    localStorage.setItem("tranova-language", "en");
    localStorage.setItem("tranova-translate-options", JSON.stringify({ sourceLanguage: "en" }));
  });
  await page.route("**/api/bootstrap", async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    data.providers = [{ id: "markdown", name: "Mock", enabled: true, model: "mock", baseUrl: "", reasoningParser: "auto", apiKey: "", proxyMode: "none", supportsImages: false, contextSize: 4096, maxSegments: 1, maxConcurrent: 1, textTranslationModel: false }];
    await route.fulfill({ json: data });
  });
  await page.route("**/api/translate-stream", (route) => {
    expect(route.request().postDataJSON().sourceLanguage).toBe("auto");
    return route.fulfill({
      contentType: "text/event-stream",
      body: `event: done\ndata: ${JSON.stringify({ result: { translatedText: markdown } })}\n\n`,
    });
  });
  await page.goto("/");
  await page.getByPlaceholder("Enter or paste text").fill("Hello");
  await page.locator(".translate-page").getByRole("button", { name: "Translate", exact: true }).click();
  const result = page.locator(".translation-result");
  await expect(result).toHaveText(markdown);
  await expect(result.locator("h1")).toHaveCount(0);
  const toggle = page.getByRole("checkbox", { name: "Render Markdown" });
  await toggle.check();
  await expect(result.getByRole("heading", { name: "Heading" })).toBeVisible();
  await expect(result.locator("strong")).toHaveText("Bold");
  await expect(result.locator("table")).toBeVisible();
  await expect(result.locator("script")).toHaveCount(0);
  await expect(result.locator('a[href^="javascript:"]')).toHaveCount(0);
  await page.reload();
  await expect(toggle).toBeChecked();
  await toggle.uncheck();
  await page.reload();
  await expect(toggle).not.toBeChecked();
});

test("cancelled folder jobs expose finished files and resume without translating them again", async ({ request }) => {
  let firstRequests = 0;
  let slowRequests = 0;
  const server = createServer((incoming, response) => {
    let body = "";
    incoming.on("data", (chunk) => { body += chunk.toString(); });
    incoming.on("end", () => {
      if (body.includes("First file")) firstRequests += 1;
      if (body.includes("Slow file") && ++slowRequests === 1) return;
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ choices: [{ message: { content: "Translated content" } }] }));
    });
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("No mock address");
  const providerId = "resume-e2e";
  try {
    expect((await request.put("/api/providers", { data: {
      id: providerId, name: "Resume test", baseUrl: `http://127.0.0.1:${address.port}/v1`,
      model: "qwen3", reasoningParser: "qwen3", apiKey: "", proxyMode: "none", enabled: true,
      supportsImages: false, contextSize: 4096, maxSegments: 1, maxConcurrent: 1, textTranslationModel: false,
    } })).ok()).toBeTruthy();
    const submit = async (resume?: Buffer) => {
      const form = new FormData();
      form.append("folderName", "resume");
      form.append("folderFile", new File(["First file"], "first.txt"), "nested/first.txt");
      form.append("folderFile", new File(["Slow file"], "slow.txt"), "nested/slow.txt");
      if (resume) form.append("resumeFile", new Blob([new Uint8Array(resume)]), "nested/first.txt");
      form.append("options", JSON.stringify({ providerId, sourceLanguage: "en", targetLanguage: "zh-CN", outputMode: "translated" }));
      const response = await request.post("/api/translate-file", { multipart: form });
      expect(response.ok()).toBeTruthy();
      return response.json();
    };
    const job = await submit();
    await expect.poll(async () => (await (await request.get(`/api/file-jobs/${job.id}`)).json()).completedFiles?.length).toBe(1);
    const processing = await (await request.get(`/api/file-jobs/${job.id}`)).json();
    expect(processing.state).toBe("processing");
    await expect.poll(() => slowRequests).toBe(1);
    const completed = await request.get(`/api/file-jobs/${job.id}/files/0`);
    expect(completed.ok()).toBeTruthy();
    const content = await completed.body();
    expect(content.toString()).toBe("Translated content");
    expect((await request.post(`/api/file-jobs/${job.id}/cancel`)).ok()).toBeTruthy();
    const resumedJob = await submit(content);
    await expect.poll(async () => (await (await request.get(`/api/file-jobs/${resumedJob.id}`)).json()).state).toBe("completed");
    const result = await (await request.get(`/api/file-jobs/${resumedJob.id}`)).json();
    expect(result.completedFiles).toEqual(expect.arrayContaining([
      expect.objectContaining({ path: "nested/first.txt", resumed: true }),
      expect.objectContaining({ path: "nested/slow.txt", resumed: false }),
    ]));
    expect(firstRequests).toBe(1);
    expect(slowRequests).toBe(2);
    expect(result.resultFilename).toBe("resume-translated.7z");
  } finally {
    await request.delete(`/api/providers/${providerId}`);
    server.closeAllConnections();
    await new Promise<void>((done) => server.close(() => done()));
  }
});

test("RAR and browser folder translations return actual 7z archives", async ({ page, request }, testInfo) => {
  const server = createServer((incoming, response) => {
    incoming.resume();
    incoming.on("end", () => {
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ choices: [{ message: { content: "Translated content" } }] }));
    });
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("No mock address");
  const providerId = "archive-e2e";
  try {
    const saved = await request.put("/api/providers", { data: {
      id: providerId, name: "Archive test", baseUrl: `http://127.0.0.1:${address.port}/v1`,
      model: "qwen3", reasoningParser: "qwen3", apiKey: "", proxyMode: "none", enabled: true,
      supportsImages: false, contextSize: 4096, maxSegments: 1, maxConcurrent: 1, textTranslationModel: false,
    } });
    expect(saved.ok()).toBeTruthy();
    const options = { providerId, sourceLanguage: "en", targetLanguage: "zh-CN", outputMode: "translated" };
    const submitted = await request.post("/api/translate-file", { multipart: {
      file: { name: "notes.rar", mimeType: "application/vnd.rar", buffer: await readFile(resolve("src-tauri/tests/fixtures/notes.rar")) },
      options: JSON.stringify(options),
    } });
    expect(submitted.ok()).toBeTruthy();
    const rarJob = await submitted.json();
    await expect.poll(async () => (await (await request.get(`/api/file-jobs/${rarJob.id}`)).json()).state).toBe("completed");
    const rarResult = await (await request.get(`/api/file-jobs/${rarJob.id}`)).json();
    expect(rarResult.resultFilename).toBe("notes-translated.7z");
    expect(rarResult.translatedSegments).toBe(1);
    const download = await request.get(`/api/file-jobs/${rarJob.id}/download`);
    expect(download.headers()["content-type"]).toBe("application/x-7z-compressed");
    expect((await download.body()).subarray(0, 6)).toEqual(Buffer.from([0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]));

    const folder = testInfo.outputPath("sample-folder");
    await mkdir(resolve(folder, "nested"), { recursive: true });
    await writeFile(resolve(folder, "nested/notes.txt"), "Hello folder");
    await writeFile(resolve(folder, "keep.bin"), Buffer.from([0, 255]));
    await page.addInitScript(({ options }) => {
      localStorage.setItem("tranova-language", "en");
      localStorage.setItem("tranova-file-options", JSON.stringify(options));
    }, { options });
    await page.goto("/");
    await page.getByRole("button", { name: "Files", exact: true }).click();
    await page.locator("input[webkitdirectory]").setInputFiles(folder);
    await expect(page.locator(".drop-zone")).toContainText("2 files");
    const queued = page.waitForResponse((response) => response.url().endsWith("/api/translate-file") && response.request().method() === "POST");
    await page.getByRole("button", { name: "Translate file", exact: true }).click();
    const folderJob = await (await queued).json();
    await expect.poll(async () => (await (await request.get(`/api/file-jobs/${folderJob.id}`)).json()).state).toBe("completed");
    const result = await (await request.get(`/api/file-jobs/${folderJob.id}`)).json();
    expect(result.resultFilename).toBe("sample-folder-translated.7z");
    expect(result.translatedSegments).toBe(1);
    const folderDownload = await request.get(`/api/file-jobs/${folderJob.id}/download`);
    expect((await folderDownload.body()).subarray(0, 6)).toEqual(Buffer.from([0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]));
  } finally {
    await request.delete(`/api/providers/${providerId}`);
    await new Promise<void>((done, reject) => server.close((error) => error ? reject(error) : done()));
  }
});
