import fs from "node:fs/promises";
import path from "node:path";

async function readStdin() {
  const chunks = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString("utf8");
}

function classifyWechatPage(finalUrl, title, html) {
  if (finalUrl.includes("wappoc_appmsgcaptcha")) {
    return {
      ok: false,
      page_kind: "wechat_captcha",
      reason: "微信公众号页面需要验证码验证",
    };
  }
  if (html.includes("你暂无权限查看此页面内容") || html.includes("失效的验证页面")) {
    return {
      ok: false,
      page_kind: "wechat_permission_denied",
      reason: "微信公众号页面暂无访问权限",
    };
  }
  if (title.includes("未知错误") || html.includes("未知错误，请稍后再试")) {
    return {
      ok: false,
      page_kind: "wechat_error",
      reason: "微信公众号页面返回错误页",
    };
  }
  if (html.includes('id="js_content"') || html.includes("js_content")) {
    return {
      ok: true,
      page_kind: "article",
      reason: null,
    };
  }
  return {
    ok: false,
    page_kind: "unknown",
    reason: "浏览器已打开页面，但未识别为正文页",
  };
}

function writeResponse(req, result, logs = []) {
  process.stdout.write(
    JSON.stringify({
      ok: result.ok,
      page_kind: result.page_kind,
      final_url: result.final_url ?? req.url,
      title: result.title ?? null,
      html_path: result.html_path ?? req.html_path,
      screenshot_path: result.screenshot_path ?? req.screenshot_path,
      reason: result.reason ?? null,
      logs,
    })
  );
}

async function hydrateLazyImages(page, logs) {
  const result = await page.evaluate(() => {
    const candidates = Array.from(document.querySelectorAll("img"));
    let patched = 0;

    for (const img of candidates) {
      const lazySrc =
        img.getAttribute("data-src") ||
        img.getAttribute("data-original") ||
        img.getAttribute("data-actualsrc") ||
        img.getAttribute("data-lazy-src") ||
        img.getAttribute("data-url");
      if (lazySrc && img.getAttribute("src") !== lazySrc) {
        img.setAttribute("src", lazySrc);
        patched += 1;
      }

      const lazySrcSet =
        img.getAttribute("data-srcset") || img.getAttribute("data-lazy-srcset");
      if (lazySrcSet && img.getAttribute("srcset") !== lazySrcSet) {
        img.setAttribute("srcset", lazySrcSet);
      }

      img.loading = "eager";
      img.decoding = "sync";
    }

    return {
      total: candidates.length,
      patched,
    };
  });

  logs.push(`images_hydrated:total=${result.total},patched=${result.patched}`);
}

async function scrollPageForLazyContent(page, logs) {
  const metrics = await page.evaluate(() => ({
    scrollHeight: Math.max(
      document.body?.scrollHeight || 0,
      document.documentElement?.scrollHeight || 0
    ),
    innerHeight: window.innerHeight || 0,
  }));

  const step = Math.max(Math.floor(metrics.innerHeight * 0.8), 400);
  let position = 0;
  let hops = 0;

  while (position + metrics.innerHeight < metrics.scrollHeight) {
    position = Math.min(position + step, metrics.scrollHeight);
    await page.evaluate((value) => window.scrollTo(0, value), position);
    await page.waitForTimeout(250);
    hops += 1;
  }

  await page.waitForTimeout(500);
  await page.evaluate(() => window.scrollTo(0, 0));
  await page.waitForTimeout(250);
  logs.push(`scroll_completed:hops=${hops},height=${metrics.scrollHeight}`);
}

async function waitForImageRendering(page, logs, timeoutMs) {
  const budgetMs = Math.max(Math.min(timeoutMs, 15000), 4000);

  const result = await page.evaluate(async (limit) => {
    const images = Array.from(document.querySelectorAll("img"));
    const deadline = Date.now() + limit;
    let loaded = 0;
    let failed = 0;

    async function waitForImage(img) {
      if (img.complete && img.naturalWidth > 0) {
        loaded += 1;
        return;
      }

      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        failed += 1;
        return;
      }

      await new Promise((resolve) => {
        let done = false;
        const finish = (ok) => {
          if (done) return;
          done = true;
          if (ok) {
            loaded += 1;
          } else {
            failed += 1;
          }
          img.removeEventListener("load", onLoad);
          img.removeEventListener("error", onError);
          clearTimeout(timer);
          resolve();
        };
        const onLoad = () => finish(true);
        const onError = () => finish(false);
        const timer = setTimeout(() => finish(false), remaining);

        img.addEventListener("load", onLoad, { once: true });
        img.addEventListener("error", onError, { once: true });

        if (typeof img.decode === "function") {
          img
            .decode()
            .then(() => finish(true))
            .catch(() => {});
        }
      });
    }

    for (const img of images) {
      await waitForImage(img);
    }

    return {
      total: images.length,
      loaded,
      failed,
    };
  }, budgetMs);

  logs.push(
    `images_ready:total=${result.total},loaded=${result.loaded},failed=${result.failed},budget_ms=${budgetMs}`
  );
}

async function preparePageForScreenshot(page, logs, timeoutMs) {
  const selectorTimeout = Math.max(Math.min(timeoutMs, 15000), 3000);
  try {
    await page.waitForSelector("#js_content, body", {
      timeout: selectorTimeout,
      state: "attached",
    });
  } catch (error) {
    logs.push(`content_selector_timeout:${error.message}`);
  }

  await hydrateLazyImages(page, logs);
  await scrollPageForLazyContent(page, logs);
  await hydrateLazyImages(page, logs);
  await waitForImageRendering(page, logs, timeoutMs);
  await page.waitForTimeout(800);
}

async function main() {
  const input = await readStdin();
  const req = JSON.parse(input);
  const logs = [];

  let playwright;
  try {
    playwright = await import("playwright");
  } catch (error) {
    writeResponse(
      req,
      {
        ok: false,
        page_kind: "browser_worker_unavailable",
        reason: `Playwright 不可用: ${error.message}`,
      },
      logs
    );
    return;
  }

  await fs.mkdir(path.dirname(req.html_path), { recursive: true });
  await fs.mkdir(path.dirname(req.screenshot_path), { recursive: true });

  let browser;
  try {
    browser = await playwright.chromium.launch({
      headless: req.headless,
    });
  } catch (error) {
    logs.push(`launch_failed:${error.message}`);
    writeResponse(
      req,
      {
        ok: false,
        page_kind: "browser_launch_failed",
        reason: `启动 Chromium 失败: ${error.message}`,
      },
      logs
    );
    return;
  }

  try {
    let context;
    try {
      context = await browser.newContext({
        viewport: req.mobile_viewport
          ? { width: 430, height: 932 }
          : { width: 1440, height: 1080 },
        isMobile: Boolean(req.mobile_viewport),
        userAgent: req.mobile_viewport
          ? "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1"
          : undefined,
      });
    } catch (error) {
      logs.push(`context_failed:${error.message}`);
      writeResponse(
        req,
        {
          ok: false,
          page_kind: "browser_context_failed",
          reason: `创建浏览器上下文失败: ${error.message}`,
        },
        logs
      );
      return;
    }

    try {
      const page = await context.newPage();
      logs.push(`goto_start:${req.url}`);
      try {
        await page.goto(req.url, {
          timeout: req.timeout_ms,
          waitUntil: "domcontentloaded",
        });
      } catch (error) {
        const pageKind =
          error.name === "TimeoutError"
            ? "browser_navigation_timeout"
            : "browser_navigation_failed";
        logs.push(`${pageKind}:${error.message}`);
        writeResponse(
          req,
          {
            ok: false,
            page_kind: pageKind,
            reason:
              error.name === "TimeoutError"
                ? `页面加载超时: ${error.message}`
                : `页面加载失败: ${error.message}`,
            final_url: page.url(),
          },
          logs
        );
        return;
      }

      await page.waitForTimeout(1500);
      await preparePageForScreenshot(page, logs, req.timeout_ms);

      const finalUrl = page.url();
      logs.push(`goto_ok:${finalUrl}`);

      let title;
      let html;
      try {
        title = await page.title();
        html = await page.content();
      } catch (error) {
        logs.push(`content_failed:${error.message}`);
        writeResponse(
          req,
          {
            ok: false,
            page_kind: "browser_content_failed",
            reason: `读取页面内容失败: ${error.message}`,
            final_url: finalUrl,
          },
          logs
        );
        return;
      }

      await fs.writeFile(req.html_path, html, "utf8");
      logs.push(`html_saved:${req.html_path}`);

      try {
        await page.screenshot({
          path: req.screenshot_path,
          fullPage: true,
        });
        logs.push(`screenshot_saved:${req.screenshot_path}`);
      } catch (error) {
        logs.push(`screenshot_failed:${error.message}`);
        writeResponse(
          req,
          {
            ok: false,
            page_kind: "browser_screenshot_failed",
            reason: `截图失败: ${error.message}`,
            final_url: finalUrl,
            title: title || null,
          },
          logs
        );
        return;
      }

      const result = classifyWechatPage(finalUrl, title, html);
      writeResponse(
        req,
        {
          ok: result.ok,
          page_kind: result.page_kind,
          final_url: finalUrl,
          title: title || null,
          reason: result.reason,
        },
        logs
      );
    } finally {
      await context.close();
    }
  } finally {
    await browser.close();
  }
}

main().catch((error) => {
  process.stdout.write(
    JSON.stringify({
      ok: false,
      page_kind: "browser_worker_failed",
      final_url: "",
      title: null,
      html_path: "",
      screenshot_path: "",
      reason: `浏览器 worker 失败: ${error.message}`,
      logs: [`uncaught:${error.message}`],
    })
  );
});
