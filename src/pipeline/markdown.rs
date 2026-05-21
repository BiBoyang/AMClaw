pub(crate) fn html_fragment_to_markdown(fragment: &str) -> String {
    let (fragment, placeholders, nonce) = replace_pre_blocks(fragment);
    let fragment = replace_anchor_blocks(&fragment);
    let fragment = replace_img_tags(&fragment);
    let result = normalize_fragment_text(&fragment);
    restore_code_placeholders(&result, &placeholders, &nonce)
}

/// Replaces `<pre>...</pre>` blocks with `__AMCLAW_CODE_{nonce}_{N}__` placeholders.
/// Returns (transformed_fragment, vec_of_code_block_strings, nonce).
pub(crate) fn replace_pre_blocks(fragment: &str) -> (String, Vec<String>, String) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string();
    let mut out = String::with_capacity(fragment.len());
    let mut placeholders: Vec<String> = Vec::new();
    let mut cursor = 0;
    let lower = fragment.to_ascii_lowercase();

    loop {
        let Some(rel_start) = lower[cursor..].find("<pre") else {
            break;
        };
        let start = cursor + rel_start;
        let after_tag = &fragment[start + 4..];
        if !after_tag.starts_with('>') && !after_tag.starts_with(|c: char| c.is_whitespace()) {
            out.push_str(&fragment[cursor..start + 4]);
            cursor = start + 4;
            continue;
        }

        out.push_str(&fragment[cursor..start]);

        let Some(rel_tag_end) = fragment[start..].find('>') else {
            out.push_str(&fragment[start..]);
            return (out, placeholders, nonce);
        };
        let tag_end = start + rel_tag_end;

        let Some(rel_close) = lower[tag_end + 1..].find("</pre>") else {
            out.push_str(&fragment[start..]);
            return (out, placeholders, nonce);
        };
        let close = tag_end + 1 + rel_close;

        let inner = &fragment[tag_end + 1..close];
        let inner = inner.replace("</code>", "\n");
        let text = strip_html_tags(&inner);
        let text = decode_entities_in_text(&text);

        let code = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        if !code.is_empty() {
            let idx = placeholders.len();
            placeholders.push(format!("\n\n```\n{}\n```\n\n", code));
            out.push_str(&format!("__AMCLAW_CODE_{nonce}_{idx}__"));
        }

        cursor = close + 6;
    }

    out.push_str(&fragment[cursor..]);
    (out, placeholders, nonce)
}

pub(crate) fn restore_code_placeholders(
    text: &str,
    placeholders: &[String],
    nonce: &str,
) -> String {
    let mut result = text.to_string();
    for (i, block) in placeholders.iter().enumerate() {
        result = result.replace(&format!("__AMCLAW_CODE_{nonce}_{i}__"), block);
    }
    result
}

pub(crate) fn strip_html_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        if ch == '<' {
            in_tag = true;
            continue;
        }
        if ch == '>' {
            in_tag = false;
            continue;
        }
        if !in_tag {
            out.push(ch);
        }
    }
    out
}

pub(crate) fn decode_entities_in_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_entity = false;
    let mut entity = String::new();

    for ch in text.chars() {
        if ch == '&' {
            in_entity = true;
            entity.clear();
            continue;
        }
        if in_entity {
            if ch == ';' {
                out.push_str(&decode_html_entity(&entity));
                in_entity = false;
            } else {
                entity.push(ch);
            }
            continue;
        }
        out.push(ch);
    }

    // If there's an unclosed entity at the end, preserve it as-is
    if in_entity {
        out.push('&');
        out.push_str(&entity);
    }

    out
}

fn normalize_fragment_text(fragment: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut in_entity = false;
    let mut entity = String::new();

    for ch in fragment.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                out.push('\n');
            }
            continue;
        }
        if in_entity {
            if ch == ';' {
                out.push_str(&decode_html_entity(&entity));
                entity.clear();
                in_entity = false;
            } else {
                entity.push(ch);
            }
            continue;
        }
        match ch {
            '<' => in_tag = true,
            '&' => in_entity = true,
            _ => out.push(ch),
        }
    }

    out.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn replace_anchor_blocks(fragment: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;

    while let Some(rel_start) = fragment[cursor..].find("<a") {
        let start = cursor + rel_start;
        out.push_str(&fragment[cursor..start]);

        let Some(rel_tag_end) = fragment[start..].find('>') else {
            out.push_str(&fragment[start..]);
            return out;
        };
        let tag_end = start + rel_tag_end;
        let tag = &fragment[start..=tag_end];
        let href = extract_attribute_value(tag, "href");
        let Some(rel_close) = fragment[tag_end + 1..].find("</a>") else {
            out.push_str(&fragment[start..]);
            return out;
        };
        let close = tag_end + 1 + rel_close;
        let inner = &fragment[tag_end + 1..close];
        let text = normalize_fragment_text(inner);

        if let Some(href) = href.filter(|href| !href.trim().is_empty()) {
            if text.is_empty() {
                out.push_str(&format!("\n\n{href}\n\n"));
            } else {
                out.push_str(&format!("\n\n{text} ({href})\n\n"));
            }
        } else {
            out.push_str(inner);
        }

        cursor = close + 4;
    }

    out.push_str(&fragment[cursor..]);
    out
}

fn replace_img_tags(fragment: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0;

    while let Some(rel_start) = fragment[cursor..].find("<img") {
        let start = cursor + rel_start;
        out.push_str(&fragment[cursor..start]);

        let Some(rel_end) = fragment[start..].find('>') else {
            out.push_str(&fragment[start..]);
            return out;
        };
        let end = start + rel_end;
        let tag = &fragment[start..=end];
        let src = extract_attribute_value(tag, "data-src")
            .or_else(|| extract_attribute_value(tag, "src"))
            .unwrap_or_default();

        if !src.trim().is_empty() {
            out.push_str(&format!("\n\n![image]({src})\n\n"));
        }

        cursor = end + 1;
    }

    out.push_str(&fragment[cursor..]);
    out
}

fn extract_attribute_value(tag: &str, attr: &str) -> Option<String> {
    ['"', '\''].into_iter().find_map(|quote| {
        let pattern = format!("{attr}={quote}");
        let start = tag.find(&pattern).map(|idx| idx + pattern.len())?;
        let end = tag[start..].find(quote).map(|idx| idx + start)?;
        Some(tag[start..end].to_string())
    })
}

pub(crate) fn decode_html_entity(entity: &str) -> String {
    match entity {
        "nbsp" => " ".to_string(),
        "amp" => "&".to_string(),
        "lt" => "<".to_string(),
        "gt" => ">".to_string(),
        "quot" => "\"".to_string(),
        "apos" => "'".to_string(),
        "#39" => "'".to_string(),
        _ => {
            if let Some(hex) = entity.strip_prefix("#x") {
                if let Ok(code) = u32::from_str_radix(hex, 16) {
                    if let Some(c) = char::from_u32(code) {
                        return c.to_string();
                    }
                }
            } else if let Some(dec) = entity.strip_prefix('#') {
                if let Ok(code) = dec.parse::<u32>() {
                    if let Some(c) = char::from_u32(code) {
                        return c.to_string();
                    }
                }
            }
            // Preserve unknown entities as-is
            format!("&{entity};")
        }
    }
}
