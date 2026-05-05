use crate::task_store::TaskStoreError;
use anyhow::Result;
use reqwest::Url;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

pub fn normalize_url(input: &str) -> Result<String> {
    let mut url = Url::parse(input).map_err(|_| TaskStoreError::InvalidUrl(input.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(TaskStoreError::UnsupportedScheme(input.to_string()).into());
    }
    // 安全：拒绝内网/本地地址，防止 SSRF
    if let Some(host) = url.host_str() {
        if is_private_host(host) {
            return Err(TaskStoreError::PrivateNetworkUrl(input.to_string()).into());
        }
    }
    url.set_fragment(None);
    strip_tracking_query_pairs(&mut url);
    let mut normalized = url.to_string();
    if url.path() == "/" && url.query().is_none() && normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(normalized)
}

/// 公开：检查 URL 是否指向私有/本地/元数据地址
pub fn is_private_url(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    parsed.host_str().is_some_and(is_private_host)
}

/// 检查 host 是否属于私有/本地/元数据地址
fn is_private_host(host: &str) -> bool {
    is_private_host_with(host, resolve_host_ips)
}

pub fn is_private_host_with<F>(host: &str, mut resolver: F) -> bool
where
    F: FnMut(&str) -> Vec<IpAddr>,
{
    // IPv6 地址在 URL 中可能带方括号 [::1]；域名可能带尾部 dot，统一清洗
    let trimmed = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.');
    let lower = trimmed.to_ascii_lowercase();
    if lower.is_empty() {
        return true;
    }

    // 特殊域名：固定视为本地/内网
    if lower == "localhost"
        || lower == "0.0.0.0"
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
        || lower.ends_with(".localhost")
        || lower == "metadata.google.internal"
    {
        return true;
    }

    // IP 字面量：按真实 IP 分类，避免基于字符串前缀误杀（如 fc*.example.com）
    if let Ok(ip) = lower.parse::<IpAddr>() {
        return is_private_ip(ip);
    }

    // 尝试解析为 IPv4 非标准表示（十六/八进制、单段/双段/三段）
    if let Some(v4) = parse_ipv4_address(&lower) {
        return is_private_ipv4(&v4.octets());
    }

    // DNS 解析防护：若域名解析到任一内网/本地地址，则视为私网
    resolver(&lower).into_iter().any(is_private_ip)
}

fn resolve_host_ips(host: &str) -> Vec<IpAddr> {
    let Ok(addrs) = format!("{host}:80").to_socket_addrs() else {
        return vec![];
    };
    addrs.map(|value| value.ip()).collect()
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_private_ipv4(&v4.octets()),
        IpAddr::V6(v6) => is_private_ipv6(&v6),
    }
}

fn is_private_ipv6(ip: &Ipv6Addr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return true;
    }
    if ip.is_unique_local() || ip.is_unicast_link_local() {
        return true;
    }
    if let Some(mapped_v4) = ip.to_ipv4_mapped() {
        return is_private_ipv4(&mapped_v4.octets());
    }
    false
}

/// 尝试从 host 字符串解析出 IPv4 地址。
/// 支持十进制、八进制(0前缀)、十六进制(0x前缀)及混合表示，
/// 兼容 a, a.b, a.b.c, a.b.c.d 四种 inet-aton 样式。
fn parse_ipv4_address(host: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = host.split('.').collect();
    match parts.len() {
        1 => {
            let value = parse_ip_number(parts[0])?;
            Some(Ipv4Addr::from(value))
        }
        2 => {
            let a = parse_ip_number(parts[0])?;
            let b = parse_ip_number(parts[1])?;
            if a > 0xff || b > 0x00ff_ffff {
                return None;
            }
            Some(Ipv4Addr::from((a << 24) | b))
        }
        3 => {
            let a = parse_ip_number(parts[0])?;
            let b = parse_ip_number(parts[1])?;
            let c = parse_ip_number(parts[2])?;
            if a > 0xff || b > 0xff || c > 0x0000_ffff {
                return None;
            }
            Some(Ipv4Addr::from((a << 24) | (b << 16) | c))
        }
        4 => {
            let mut octets = [0u8; 4];
            for (i, part) in parts.iter().enumerate() {
                octets[i] = u8::try_from(parse_ip_number(part)?).ok()?;
            }
            Some(Ipv4Addr::from(octets))
        }
        _ => None,
    }
}

/// 解析 IP 数字段：支持十进制、八进制(0前缀)、十六进制(0x前缀)
fn parse_ip_number(s: &str) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else if s.len() > 1 && s.starts_with('0') {
        u32::from_str_radix(s, 8).ok()
    } else {
        s.parse::<u32>().ok()
    }
}

/// 判断 IPv4 八位组是否属于私有/保留地址
fn is_private_ipv4(octets: &[u8; 4]) -> bool {
    match octets[0] {
        0 => true,                                      // 0.0.0.0/8
        10 => true,                                     // 10.0.0.0/8
        100 if (64..=127).contains(&octets[1]) => true, // 100.64.0.0/10 (CGN)
        127 => true,                                    // 127.0.0.0/8
        169 if octets[1] == 254 => true,                // 169.254.0.0/16 (link-local / 云元数据)
        172 if (16..=31).contains(&octets[1]) => true,  // 172.16.0.0/12
        192 => match octets[1] {
            0 if octets[2] == 0 => true, // 192.0.0.0/24
            168 => true,                 // 192.168.0.0/16
            _ => false,
        },
        198 if octets[1] == 51 && octets[2] == 100 => true, // 198.51.100.0/24 (文档)
        203 if octets[1] == 0 && octets[2] == 113 => true,  // 203.0.113.0/24 (文档)
        224..=239 => true,                                  // 组播
        240..=255 => true,                                  // 保留
        _ => false,
    }
}

fn strip_tracking_query_pairs(url: &mut Url) {
    let tracking_keys: HashSet<&str> = [
        "fbclid", "gclid", "mc_cid", "mc_eid", "mkt_tok", "spm", "si",
    ]
    .into_iter()
    .collect();

    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.as_ref();
            !key.starts_with("utm_") && !tracking_keys.contains(key)
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();

    url.set_query(None);
    if pairs.is_empty() {
        return;
    }

    let mut query_pairs = url.query_pairs_mut();
    for (key, value) in pairs {
        query_pairs.append_pair(&key, &value);
    }
}

pub fn source_domain(normalized_url: &str) -> Option<String> {
    Url::parse(normalized_url)
        .ok()
        .and_then(|url| url.host_str().map(ToOwned::to_owned))
}
