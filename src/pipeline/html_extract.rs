use super::markdown::html_fragment_to_markdown;
use super::ExtractedArchiveBody;

pub(crate) fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title>")?;
    let end = lower[start + 7..].find("</title>")?;
    let raw = &html[start + 7..start + 7 + end];
    let title = raw.replace(['\n', '\r'], " ");
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        None
    } else {
        Some(title)
    }
}

pub(crate) fn preview_text(html: &str) -> String {
    let text = html
        .replace(['\n', '\r'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut preview: String = text.chars().take(240).collect();
    if text.chars().count() > 240 {
        preview.push_str("...");
    }
    preview
}

pub(crate) fn extract_primary_body(html: &str) -> Option<String> {
    if let Some(wechat_body) = extract_wechat_primary_body(html) {
        return Some(wechat_body);
    }

    extract_generic_primary_body(html)
}

pub(crate) fn extract_wechat_primary_body(html: &str) -> Option<String> {
    let activity_title = extract_element_text_by_id(html, "activity-name");
    let body = extract_element_inner_html_by_id(html, "js_content")
        .map(|fragment| html_fragment_to_markdown(&fragment))
        .filter(|text| !text.trim().is_empty());

    match (activity_title, body) {
        (Some(title), Some(body)) => Some(format!("## {}\n\n{}", title.trim(), body.trim())),
        (None, Some(body)) => Some(body.trim().to_string()),
        (Some(title), None) => Some(format!("## {}", title.trim())),
        (None, None) => None,
    }
}

fn extract_generic_primary_body(html: &str) -> Option<String> {
    for tag in ["article", "main"] {
        if let Some(body) = extract_element_inner_html_by_tag(html, tag)
            .map(|fragment| html_fragment_to_markdown(&fragment))
            .map(|text| text.trim().to_string())
            .filter(|text| is_meaningful_extracted_body(text))
        {
            return Some(body);
        }
    }

    extract_element_inner_html_by_tag(html, "body")
        .map(|fragment| html_fragment_to_markdown(&fragment))
        .map(|text| text.trim().to_string())
        .filter(|text| is_meaningful_extracted_body(text))
}

fn extract_element_text_by_id(html: &str, element_id: &str) -> Option<String> {
    extract_element_inner_html_by_id(html, element_id)
        .map(|fragment| html_fragment_to_markdown(&fragment))
}

fn extract_element_inner_html_by_id(html: &str, element_id: &str) -> Option<String> {
    let id_patterns = [format!("id=\"{element_id}\""), format!("id='{element_id}'")];
    let mut start_idx = None;
    for pattern in id_patterns {
        if let Some(found) = html.find(&pattern) {
            start_idx = Some(found);
            break;
        }
    }
    let start_idx = start_idx?;
    let tag_open_start = html[..start_idx].rfind('<')?;
    let tag_open_end = html[start_idx..].find('>')? + start_idx;
    let tag_name = html[tag_open_start + 1..]
        .split_whitespace()
        .next()?
        .trim_start_matches('/')
        .trim_end_matches('>');
    let close_tag = format!("</{tag_name}>");
    let content_start = tag_open_end + 1;
    let content_end = html[content_start..].find(&close_tag)? + content_start;
    html.get(content_start..content_end).map(ToOwned::to_owned)
}

fn extract_element_inner_html_by_tag(html: &str, tag_name: &str) -> Option<String> {
    let open_pattern = format!("<{tag_name}");
    let start_idx = html.to_ascii_lowercase().find(&open_pattern)?;
    let tag_open_end = html[start_idx..].find('>')? + start_idx;
    let close_tag = format!("</{tag_name}>");
    let content_start = tag_open_end + 1;
    let content_end = html[content_start..]
        .to_ascii_lowercase()
        .find(&close_tag)?
        + content_start;
    html.get(content_start..content_end).map(ToOwned::to_owned)
}

pub(crate) fn extract_http_archive_body(html: &str) -> ExtractedArchiveBody {
    if let Some(markdown) = extract_primary_body(html) {
        let page_kind = classify_http_page_kind(html, &markdown);
        return ExtractedArchiveBody {
            markdown,
            page_kind,
            section_title: "Content",
        };
    }

    let preview = preview_text(html);
    let page_kind = classify_http_page_kind(html, &preview);
    ExtractedArchiveBody {
        markdown: preview,
        page_kind,
        section_title: "Preview",
    }
}

pub(crate) fn classify_http_page_kind(html: &str, markdown: &str) -> String {
    let lower = html.to_ascii_lowercase();

    if is_error_page(html, &lower) {
        return "error_page".to_string();
    }
    if looks_like_article(html, &lower, markdown) {
        return "article".to_string();
    }
    if is_index_like_page(&lower, markdown) {
        return "index_like".to_string();
    }
    if is_link_post(markdown) {
        return "link_post".to_string();
    }

    "webpage".to_string()
}

fn is_error_page(html: &str, lower_html: &str) -> bool {
    // Pages with <article> or og:type=article are not error pages
    if lower_html.contains("<article")
        || lower_html.contains("property=\"og:type\" content=\"article")
        || lower_html.contains("property='og:type' content='article")
    {
        return false;
    }
    let body_text_len = lower_html
        .split("<body")
        .last()
        .map(|rest| {
            rest.chars()
                .filter(|c| !c.is_whitespace() && *c != '<' && *c != '>' && *c != '/')
                .count()
        })
        .unwrap_or(0);
    if body_text_len > 1500 {
        return false;
    }
    let title = extract_html_title(html)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let error_keywords = [
        "404",
        "not found",
        "页面不存在",
        "找不到页面",
        "page not found",
        "页面未找到",
    ];
    error_keywords.iter().any(|k| title.contains(k))
}

fn is_index_like_page(lower_html: &str, markdown: &str) -> bool {
    let link_count = lower_html.matches("<a ").count();
    let prose_chars = markdown.chars().count();
    if link_count >= 10 && prose_chars < 500 {
        return true;
    }
    let li_count = lower_html.matches("<li").count();
    li_count >= 8 && prose_chars < 800
}

fn is_link_post(markdown: &str) -> bool {
    let prose_chars = markdown.chars().count();
    prose_chars < 300 && markdown.contains("http")
}

fn looks_like_article(_html: &str, lower_html: &str, markdown: &str) -> bool {
    let paragraph_count = markdown.matches("\n\n").count() + 1;
    let body_chars = markdown.chars().count();
    lower_html.contains("<article")
        || lower_html.contains("property=\"og:type\" content=\"article")
        || lower_html.contains("property='og:type' content='article")
        || lower_html.contains("name=\"twitter:card\" content=\"summary_large_image")
        || paragraph_count >= 3
        || body_chars >= 400
}

pub(crate) fn is_meaningful_extracted_body(text: &str) -> bool {
    let non_empty_lines = text.lines().filter(|line| !line.trim().is_empty()).count();
    let chars = text.chars().count();
    chars >= 80 || non_empty_lines >= 3
}

pub(crate) fn generate_rule_summary(title: Option<&str>, markdown: &str) -> Option<String> {
    let paragraphs: Vec<&str> = markdown
        .split("\n\n")
        .map(|p| p.trim())
        .filter(|p| is_meaningful_summary_paragraph(p))
        .take(3)
        .collect();

    if paragraphs.is_empty() {
        return None;
    }

    let mut lines: Vec<String> = Vec::new();

    if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
        lines.push(truncate_line(t, 200));
    }

    for p in &paragraphs {
        let first_line = p.lines().next().unwrap_or(p);
        lines.push(truncate_line(first_line, 150));
    }

    if lines.is_empty() {
        return None;
    }

    Some(lines.join("\n"))
}

pub(crate) fn is_meaningful_summary_paragraph(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().count() < 20 {
        return false;
    }
    !is_navigation_like(trimmed)
}

pub(crate) fn is_navigation_like(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let short = text.chars().count() < 60;
    let nav_keywords = [
        "版权所有",
        "备案号",
        "icp",
        "关注我们",
        "扫码关注",
        "点击阅读原文",
        "分享到",
        "copyright",
        "all rights reserved",
        "cookie",
        "privacy policy",
        "隐私政策",
        "回到顶部",
        "返回顶部",
        "top ↑",
    ];
    if short && nav_keywords.iter().any(|k| lower.contains(k)) {
        return true;
    }
    if lower.starts_with("http") && !lower.contains(' ') {
        return true;
    }
    false
}

pub(crate) fn truncate_line(text: &str, max_chars: usize) -> String {
    let cleaned: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        format!("{cleaned}...")
    } else {
        cleaned
    }
}
