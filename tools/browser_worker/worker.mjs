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

async function main() {
  const input = await readStdin();
  const req = JSON.parse(input);

  let playwright;
  try {
    playwright = await import("playwright");
  } catch (error) {
    process.stdout.write(
      JSON.stringify({
        ok: false,
        page_kind: "browser_worker_unavailable",
        final_url: req.url,
        title: null,
        html_path: req.html_path,
        screenshot_path: req.screenshot_path,
        reason: `Playwright 不可用: ${error.message}`,
      })
    );
    return;
  }

  await fs.mkdir(path.dirname(req.html_path), { recursive: true });
  await fs.mkdir(path.dirname(req.screenshot_path), { recursive: true });

  const browser = await playwright.chromium.launch({
    headless: req.headless,
  });

  try {
    const context = await browser.newContext({
      viewport: req.mobile_viewport
        ? { width: 430, height: 932 }
        : { width: 1440, height: 1080 },
      isMobile: Boolean(req.mobile_viewport),
      userAgent: req.mobile_viewport
        ? "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1"
        : undefined,
    });
    const page = await context.newPage();
    await page.goto(req.url, {
      timeout: req.timeout_ms,
      waitUntil: "domcontentloaded",
    });
    await page.waitForTimeout(1500);

    const finalUrl = page.url();
    const title = await page.title();
    const html = await page.content();
    await fs.writeFile(req.html_path, html, "utf8");
    await page.screenshot({
      path: req.screenshot_path,
      fullPage: true,
    });

    const result = classifyWechatPage(finalUrl, title, html);
    process.stdout.write(
      JSON.stringify({
        ok: result.ok,
        page_kind: result.page_kind,
        final_url: finalUrl,
        title: title || null,
        html_path: req.html_path,
        screenshot_path: req.screenshot_path,
        reason: result.reason,
      })
    );
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
    })
  );
});
