use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::Url;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::path::Path;

mod chunk_queue;
mod embedding_cache;
mod logging;
mod memory;
mod schema;
mod sessions;
mod tasks;
mod types;
mod url_guard;

pub use self::types::{
    ArchivedTaskRecord, ClaimableTaskRecord, FeedbackKind, LinkTaskRecord, MarkTaskArchivedInput,
    MemoryFeedbackState, MemoryType, MemoryWriteState, PendingChunkRecord, PendingTaskRecord,
    PromoteReason, RecentTaskRecord, SkipReason, StoredSessionRecord, TaskContentRecord,
    TaskStatusRecord, TaskStoreError, UserMemoryRecord, UserSessionStateRecord, WriteDecision,
};

/// 最大单条内容长度（写入时校验）

#[derive(Debug)]
pub struct TaskStore {
    conn: Connection,
}

impl TaskStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建数据库目录失败: {}", parent.display()))?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("打开 SQLite 数据库失败: {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_millis(5000))
            .with_context(|| format!("设置 SQLite busy_timeout 失败: {}", path.display()))?;
        for attempt in 0..5 {
            match conn.pragma_update(None, "journal_mode", "WAL") {
                Ok(_) => break,
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    if attempt == 4 {
                        return Err(rusqlite::Error::SqliteFailure(err, None)).with_context(|| {
                            format!("设置 SQLite WAL 模式失败: {}", path.display())
                        });
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("设置 SQLite WAL 模式失败: {}", path.display()));
                }
            }
        }
        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    // ---- Outbound Pending Chunks API ----

    /// 将剩余未发送的消息段持久化，供后续补发。
    pub fn insert_pending_chunks(
        &mut self,
        user_id: &str,
        context_token: &str,
        chunks: &[(usize, usize, String)],
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction().context("开启 chunks 事务失败")?;
        for (chunk_index, chunk_total, chunk_text) in chunks {
            tx.execute(
                r#"
                INSERT INTO outbound_pending_chunks
                (user_id, context_token, chunk_text, chunk_index, chunk_total, created_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
                params![
                    user_id,
                    context_token,
                    chunk_text,
                    *chunk_index as i64,
                    *chunk_total as i64,
                    now.clone(),
                ],
            )
            .context("插入 pending chunk 失败")?;
        }
        tx.commit().context("提交 chunks 事务失败")?;
        log_task_store_info(
            "pending_chunks_inserted",
            vec![
                ("user_id", json!(user_id)),
                ("chunk_count", json!(chunks.len())),
            ],
        );
        Ok(())
    }

    /// 查询最早的一批待补发消息段（按 created_at 排序）。
    pub fn list_pending_chunks(&self, limit: usize) -> Result<Vec<PendingChunkRecord>> {
        let limit = i64::try_from(limit).context("chunk limit 超出范围")?;
        let mut stmt = self
            .conn
            .prepare(
                r#"
                SELECT id, user_id, context_token, chunk_text, chunk_index, chunk_total, created_at
                FROM outbound_pending_chunks
                ORDER BY created_at ASC
                LIMIT ?1
                "#,
            )
            .context("准备 pending chunks 查询失败")?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(PendingChunkRecord {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    context_token: row.get(2)?,
                    chunk_text: row.get(3)?,
                    chunk_index: row.get::<_, usize>(4)?,
                    chunk_total: row.get::<_, usize>(5)?,
                    created_at: row.get(6)?,
                })
            })
            .context("查询 pending chunks 失败")?;
        let mut chunks = Vec::new();
        for row in rows {
            chunks.push(row.context("读取 pending chunk 记录失败")?);
        }
        Ok(chunks)
    }

    /// 删除指定待补发消息段。
    pub fn delete_pending_chunk(&mut self, id: i64) -> Result<bool> {
        let deleted = self
            .conn
            .execute("DELETE FROM outbound_pending_chunks WHERE id = ?1", [id])
            .context("删除 pending chunk 失败")?;
        Ok(deleted > 0)
    }
}

pub(super) fn log_task_store_info(event: &str, fields: Vec<(&str, Value)>) {
    log_task_store_event("info", event, fields);
}

pub(super) fn log_task_store_warn(event: &str, fields: Vec<(&str, Value)>) {
    log_task_store_event("warn", event, fields);
}

fn log_task_store_error(event: &str, fields: Vec<(&str, Value)>) {
    log_task_store_event("error", event, fields);
}

fn log_task_store_event(level: &str, event: &str, fields: Vec<(&str, Value)>) {
    crate::logging::emit_structured_log(level, event, fields);
}

#[cfg(test)]
fn build_task_store_log_payload(level: &str, event: &str, fields: Vec<(&str, Value)>) -> Value {
    crate::logging::build_structured_log_payload(level, event, fields)
}

pub(super) fn summarize_text_for_log(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut output: String = input.chars().take(max_chars).collect();
    output.push_str("...");
    output
}

pub(super) fn normalize_url(input: &str) -> Result<String> {
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

fn is_private_host_with<F>(host: &str, mut resolver: F) -> bool
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

pub(super) fn source_domain(normalized_url: &str) -> Option<String> {
    Url::parse(normalized_url)
        .ok()
        .and_then(|url| url.host_str().map(ToOwned::to_owned))
}

#[cfg(test)]
mod tests {
    use super::{
        build_task_store_log_payload, ArchivedTaskRecord, FeedbackKind, LinkTaskRecord,
        MarkTaskArchivedInput, MemoryFeedbackState, MemoryType, MemoryWriteState,
        PendingTaskRecord, PromoteReason, RecentTaskRecord, SkipReason, StoredSessionRecord,
        TaskStatusRecord, TaskStore, UserMemoryRecord, UserSessionStateRecord, WriteDecision,
    };
    use rusqlite::{params, Connection};
    use serde_json::{json, Value};
    use std::fs;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::sync::Arc;
    use std::thread;
    use uuid::Uuid;

    fn temp_db_path() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("amclaw_task_store_test_{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("创建测试目录失败");
        root.join("amclaw.db")
    }

    #[test]
    fn schema_is_created() {
        let db_path = temp_db_path();
        TaskStore::open(&db_path).expect("初始化 task store 失败");

        let conn = Connection::open(&db_path).expect("打开数据库失败");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'inbound_messages'",
                [],
                |row| row.get(0),
            )
            .expect("查询表结构失败");

        assert_eq!(count, 1);
    }

    #[test]
    fn duplicate_message_is_ignored_even_after_reopen() {
        let db_path = temp_db_path();

        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        assert!(store
            .record_inbound_message("msg-1", "user-a", "hello")
            .expect("首次写入失败"));
        assert!(!store
            .record_inbound_message("msg-1", "user-a", "hello")
            .expect("重复写入失败"));
        drop(store);

        let mut reopened = TaskStore::open(&db_path).expect("重新打开 task store 失败");
        assert!(!reopened
            .record_inbound_message("msg-1", "user-a", "hello")
            .expect("重启后重复写入失败"));
    }

    #[test]
    fn inbound_message_text_is_persisted() {
        let db_path = temp_db_path();

        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .record_inbound_message("msg-2", "user-b", "https://example.com hello")
            .expect("写入入站消息失败");
        drop(store);

        let conn = Connection::open(&db_path).expect("打开数据库失败");
        let row: (String, String, String) = conn
            .query_row(
                "SELECT message_id, from_user_id, text FROM inbound_messages WHERE message_id = 'msg-2'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("查询入站消息失败");

        assert_eq!(
            row,
            (
                "msg-2".to_string(),
                "user-b".to_string(),
                "https://example.com hello".to_string(),
            )
        );
    }

    #[test]
    fn duplicate_message_does_not_create_second_inbound_row() {
        let db_path = temp_db_path();

        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        assert!(store
            .record_inbound_message("msg-3", "user-c", "first")
            .expect("首次写入失败"));
        assert!(!store
            .record_inbound_message("msg-3", "user-c", "second")
            .expect("重复写入失败"));
        drop(store);

        let conn = Connection::open(&db_path).expect("打开数据库失败");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM inbound_messages WHERE message_id = 'msg-3'",
                [],
                |row| row.get(0),
            )
            .expect("查询入站消息数量失败");
        let text: String = conn
            .query_row(
                "SELECT text FROM inbound_messages WHERE message_id = 'msg-3'",
                [],
                |row| row.get(0),
            )
            .expect("查询入站消息文本失败");

        assert_eq!(count, 1);
        assert_eq!(text, "first");
    }

    #[test]
    fn link_submission_creates_article_and_task() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let record = store
            .record_link_submission("https://example.com/path?q=1")
            .expect("写入链接失败");
        drop(store);

        let conn = Connection::open(&db_path).expect("打开数据库失败");
        let article_row: (String, String) = conn
            .query_row(
                "SELECT id, normalized_url FROM articles WHERE id = ?1",
                [record.article_id.clone()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("查询文章失败");
        let task_row: (String, String) = conn
            .query_row(
                "SELECT id, article_id FROM tasks WHERE id = ?1",
                [record.task_id.clone()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("查询任务失败");

        assert_eq!(article_row.0, record.article_id);
        assert_eq!(article_row.1, "https://example.com/path?q=1");
        assert_eq!(task_row.0, record.task_id);
        assert_eq!(task_row.1, record.article_id);
        assert!(record.created_new);
    }

    #[test]
    fn duplicate_link_returns_existing_article_and_task() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let first = store
            .record_link_submission("https://example.com")
            .expect("首次写入链接失败");
        let second = store
            .record_link_submission("https://example.com/")
            .expect("重复写入链接失败");
        drop(store);

        let conn = Connection::open(&db_path).expect("打开数据库失败");
        let article_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles", [], |row| row.get(0))
            .expect("查询文章数量失败");
        let task_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .expect("查询任务数量失败");

        assert_eq!(
            second,
            LinkTaskRecord {
                article_id: first.article_id.clone(),
                task_id: first.task_id.clone(),
                normalized_url: "https://example.com".to_string(),
                original_url: "https://example.com/".to_string(),
                created_new: false,
            }
        );
        assert_eq!(article_count, 1);
        assert_eq!(task_count, 1);
    }

    #[test]
    fn tracking_query_params_are_removed_during_normalization() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let first = store
            .record_link_submission("https://example.com/page?utm_source=x&gclid=1&id=42")
            .expect("首次写入链接失败");
        let second = store
            .record_link_submission("https://example.com/page?id=42&utm_medium=email")
            .expect("重复写入链接失败");

        assert_eq!(first.normalized_url, "https://example.com/page?id=42");
        assert_eq!(second.normalized_url, "https://example.com/page?id=42");
        assert!(!second.created_new);
    }

    #[test]
    fn private_network_urls_are_rejected() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let private_urls = vec![
            // 经典私有段
            "http://127.0.0.1/secret",
            "http://localhost/admin",
            "http://192.168.1.1/router",
            "http://10.0.0.1/internal",
            "http://172.16.0.1/corp",
            "http://172.31.255.255/corp",
            // 云元数据 / link-local
            "http://169.254.169.254/metadata",
            "http://169.254.0.1/whatever",
            // IPv6
            "http://[::1]/secret",
            "http://[fc00::1]/internal",
            "http://[fe80::1]/link",
            // 非十进制表示
            "http://0x7f000001/secret",
            "http://0177.0.0.1/secret",
            "http://2130706433/secret",
            // 特殊域名
            "http://myapp.localhost/",
            "http://myapp.local/",
        ];
        for url in private_urls {
            let err = store
                .record_link_submission(url)
                .expect_err(&format!("应拒绝内网 URL: {url}"));
            assert!(
                err.to_string().contains("内网"),
                "错误信息应包含'内网': {} => {}",
                url,
                err
            );
        }
        // 公网 URL 应正常通过
        store
            .record_link_submission("https://example.com/public")
            .expect("公网 URL 应正常通过");
    }

    #[test]
    fn is_private_url_detects_all_known_patterns() {
        assert!(super::is_private_url(
            "http://169.254.169.254/latest/meta-data/"
        ));
        assert!(super::is_private_url("http://100.64.0.1/cgn"));
        assert!(super::is_private_url("http://0x7f000001/ping"));
        assert!(super::is_private_url("http://0177.0.0.1/ping"));
        assert!(super::is_private_url("http://[::1]/ping"));
        assert!(super::is_private_url("http://[fc00::1]/ping"));
        // 公网不应命中
        assert!(!super::is_private_url("https://example.com/page"));
        assert!(!super::is_private_url("https://1.1.1.1/dns"));
        assert!(!super::is_private_url("https://8.8.8.8/dns"));
    }

    #[test]
    fn fc_fd_prefix_domain_names_are_not_falsely_blocked() {
        assert!(!super::is_private_url("https://fc-news.example.com/page"));
        assert!(!super::is_private_url("https://fdomain.example.com/page"));
    }

    #[test]
    fn domain_resolving_to_private_ip_is_blocked() {
        assert!(super::is_private_host_with("demo.test", |host| {
            if host == "demo.test" {
                vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))]
            } else {
                vec![]
            }
        }));
        assert!(super::is_private_host_with("demo6.test", |host| {
            if host == "demo6.test" {
                vec![IpAddr::V6(Ipv6Addr::LOCALHOST)]
            } else {
                vec![]
            }
        }));
    }

    #[test]
    fn domain_resolving_to_public_ip_is_allowed() {
        assert!(!super::is_private_host_with("public.test", |host| {
            if host == "public.test" {
                vec![IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))]
            } else {
                vec![]
            }
        }));
    }

    #[test]
    fn non_http_scheme_is_rejected_during_link_submission() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let err = store
            .record_link_submission("file:///tmp/demo.html")
            .expect_err("应拒绝非 http/https 协议");

        assert!(err.to_string().contains("仅支持 http/https URL"));
    }

    #[test]
    fn javascript_scheme_is_rejected_during_link_submission() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let err = store
            .record_link_submission("javascript:alert(1)")
            .expect_err("应拒绝 javascript 协议");

        assert!(err.to_string().contains("仅支持 http/https URL"));
    }

    #[test]
    fn task_status_can_be_queried() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let created = store
            .record_link_submission("https://example.com/status")
            .expect("写入链接失败");
        let status = store
            .get_task_status(&created.task_id)
            .expect("查询任务状态失败")
            .expect("应存在任务状态");

        assert_eq!(
            status,
            TaskStatusRecord {
                task_id: created.task_id.clone(),
                article_id: created.article_id.clone(),
                normalized_url: "https://example.com/status".to_string(),
                title: None,
                content_source: None,
                page_kind: None,
                status: "pending".to_string(),
                retry_count: 0,
                last_error: None,
                output_path: None,
                snapshot_path: None,
                created_at: status.created_at.clone(),
                updated_at: status.updated_at.clone(),
            }
        );
    }

    #[test]
    fn querying_missing_task_returns_none() {
        let db_path = temp_db_path();
        let store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let status = store
            .get_task_status("missing-task")
            .expect("查询不存在任务失败");

        assert_eq!(status, None);
    }

    #[test]
    fn recent_tasks_returns_latest_first() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let first = store
            .record_link_submission("https://example.com/one")
            .expect("写入第一条链接失败");
        let second = store
            .record_link_submission("https://example.com/two")
            .expect("写入第二条链接失败");

        let tasks = store.list_recent_tasks(10).expect("查询最近任务失败");

        assert_eq!(
            tasks,
            vec![
                RecentTaskRecord {
                    task_id: second.task_id,
                    status: "pending".to_string(),
                    content_source: None,
                    page_kind: None,
                    normalized_url: "https://example.com/two".to_string(),
                    updated_at: tasks[0].updated_at.clone(),
                },
                RecentTaskRecord {
                    task_id: first.task_id,
                    status: "pending".to_string(),
                    content_source: None,
                    page_kind: None,
                    normalized_url: "https://example.com/one".to_string(),
                    updated_at: tasks[1].updated_at.clone(),
                },
            ]
        );
    }

    #[test]
    fn retry_task_resets_status_and_clears_error() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .record_link_submission("https://example.com/retry")
            .expect("写入链接失败");

        let conn = Connection::open(&db_path).expect("打开数据库失败");
        conn.execute(
            "UPDATE tasks SET status = 'failed', retry_count = 2, last_error = 'boom' WHERE id = ?1",
            [created.task_id.as_str()],
        )
        .expect("准备失败任务状态失败");
        drop(conn);

        let retried = store
            .retry_task(&created.task_id)
            .expect("重试任务失败")
            .expect("应存在任务");

        assert_eq!(retried.status, "pending");
        assert_eq!(retried.normalized_url, "https://example.com/retry");
        assert_eq!(retried.content_source, None);
        assert_eq!(retried.page_kind, None);
        assert_eq!(retried.retry_count, 3);
        assert_eq!(retried.last_error, None);
        assert_eq!(retried.output_path, None);
        assert_eq!(retried.snapshot_path, None);
    }

    #[test]
    fn retry_missing_task_returns_none() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let retried = store
            .retry_task("missing-task")
            .expect("重试不存在任务失败");

        assert_eq!(retried, None);
    }

    #[test]
    fn retry_processing_task_returns_validation_error() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .record_link_submission("https://example.com/retry-processing")
            .expect("写入链接失败");

        // 先领取，进入 processing 状态
        assert!(
            store
                .claim_task(&created.task_id, "worker-a", 300)
                .expect("claim 失败"),
            "pending 任务应可被领取"
        );

        let err = store
            .retry_task(&created.task_id)
            .expect_err("processing 状态下重试应失败");
        let message = err.to_string();
        assert!(
            message.contains("不允许重试"),
            "错误信息应提示不允许重试，实际: {message}"
        );
        assert!(
            message.contains("processing"),
            "错误信息应包含当前状态 processing，实际: {message}"
        );
    }

    #[test]
    fn expired_lease_task_can_be_reclaimed() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .record_link_submission("https://example.com/reclaim")
            .expect("写入链接失败");

        // 首次领取成功，第二次应失败（lease 尚未过期）
        assert!(store
            .claim_task(&created.task_id, "worker-a", 300)
            .expect("首次 claim 失败"));
        assert!(
            !store
                .claim_task(&created.task_id, "worker-b", 300)
                .expect("二次 claim 查询失败"),
            "lease 未过期时不应被再次领取"
        );

        // 人工制造过期 lease，再次领取应成功
        let conn = Connection::open(&db_path).expect("打开数据库失败");
        conn.execute(
            "UPDATE tasks SET lease_until = ?2 WHERE id = ?1",
            params![created.task_id.as_str(), "2000-01-01T00:00:00+00:00"],
        )
        .expect("回写过期 lease 失败");
        drop(conn);

        assert!(
            store
                .claim_task(&created.task_id, "worker-b", 300)
                .expect("过期后 claim 失败"),
            "lease 过期后应可被重新领取"
        );

        let conn = Connection::open(&db_path).expect("打开数据库失败");
        let worker_id: Option<String> = conn
            .query_row(
                "SELECT worker_id FROM tasks WHERE id = ?1",
                [created.task_id.as_str()],
                |row| row.get(0),
            )
            .expect("读取 worker_id 失败");
        assert_eq!(worker_id, Some("worker-b".to_string()));
    }

    #[test]
    fn pending_tasks_can_be_listed_and_archived() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .record_link_submission("https://example.com/pending")
            .expect("写入链接失败");

        let pending = store.list_pending_tasks(10).expect("查询 pending 失败");
        assert_eq!(
            pending,
            vec![PendingTaskRecord {
                task_id: created.task_id.clone(),
                article_id: created.article_id.clone(),
                normalized_url: "https://example.com/pending".to_string(),
                original_url: "https://example.com/pending".to_string(),
            }]
        );

        store
            .claim_task(&created.task_id, "test-worker", 300)
            .expect("claim 失败");

        assert!(store
            .mark_task_archived(
                &created.task_id,
                MarkTaskArchivedInput {
                    output_path: "/tmp/example.md",
                    title: Some("Example Title"),
                    page_kind: Some("article"),
                    snapshot_path: Some("/tmp/example.png"),
                    content_source: Some("browser_capture"),
                    summary: None,
                },
            )
            .expect("更新 archived 状态失败"));

        let pending_after = store.list_pending_tasks(10).expect("查询 pending 失败");
        let status = store
            .get_task_status(&created.task_id)
            .expect("查询状态失败")
            .expect("应存在任务");

        assert!(pending_after.is_empty());
        assert_eq!(status.status, "archived");
        assert_eq!(status.content_source, Some("browser_capture".to_string()));
        assert_eq!(status.page_kind, Some("article".to_string()));
        assert_eq!(status.output_path, Some("/tmp/example.md".to_string()));
        assert_eq!(status.snapshot_path, Some("/tmp/example.png".to_string()));
        assert_eq!(status.title, Some("Example Title".to_string()));
    }

    #[test]
    fn task_can_be_marked_failed() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .record_link_submission("https://example.com/fail")
            .expect("写入链接失败");

        store
            .claim_task(&created.task_id, "test-worker", 300)
            .expect("claim 失败");

        assert!(store
            .mark_task_failed(&created.task_id, "network fail")
            .expect("更新 failed 状态失败"));

        let status = store
            .get_task_status(&created.task_id)
            .expect("查询状态失败")
            .expect("应存在任务");

        assert_eq!(status.status, "failed");
        assert_eq!(status.content_source, None);
        assert_eq!(status.last_error, Some("network fail".to_string()));
    }

    #[test]
    fn task_can_be_marked_awaiting_manual_input() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .record_link_submission("https://mp.weixin.qq.com/s/manual")
            .expect("写入链接失败");

        store
            .claim_task(&created.task_id, "test-worker", 300)
            .expect("claim 失败");

        assert!(store
            .mark_task_awaiting_manual_input(
                &created.task_id,
                "微信公众号页面需要验证码验证",
                "wechat_captcha",
                None,
                Some("browser_capture"),
            )
            .expect("更新 awaiting_manual_input 状态失败"));

        let status = store
            .get_task_status(&created.task_id)
            .expect("查询状态失败")
            .expect("应存在任务");

        assert_eq!(status.status, "awaiting_manual_input");
        assert_eq!(status.content_source, Some("browser_capture".to_string()));
        assert_eq!(status.page_kind, Some("wechat_captcha".to_string()));
        assert_eq!(
            status.last_error,
            Some("微信公众号页面需要验证码验证".to_string())
        );
    }

    #[test]
    fn manual_tasks_can_be_listed() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .record_link_submission("https://mp.weixin.qq.com/s/manual-list")
            .expect("写入链接失败");

        store
            .claim_task(&created.task_id, "test-worker", 300)
            .expect("claim 失败");
        store
            .mark_task_awaiting_manual_input(
                &created.task_id,
                "微信公众号页面需要验证码验证",
                "wechat_captcha",
                None,
                Some("browser_capture"),
            )
            .expect("更新 awaiting_manual_input 状态失败");

        let tasks = store.list_manual_tasks(10).expect("查询待补录任务失败");

        assert_eq!(
            tasks,
            vec![RecentTaskRecord {
                task_id: created.task_id,
                status: "awaiting_manual_input".to_string(),
                content_source: Some("browser_capture".to_string()),
                page_kind: Some("wechat_captcha".to_string()),
                normalized_url: "https://mp.weixin.qq.com/s/manual-list".to_string(),
                updated_at: tasks[0].updated_at.clone(),
            }]
        );
    }

    #[test]
    fn archived_tasks_can_be_listed() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .record_link_submission("https://example.com/archived-list")
            .expect("写入链接失败");

        store
            .claim_task(&created.task_id, "test-worker", 300)
            .expect("claim 失败");

        assert!(store
            .mark_task_archived(
                &created.task_id,
                MarkTaskArchivedInput {
                    output_path: "/tmp/archived-list.md",
                    title: Some("Archived List Title"),
                    page_kind: Some("article"),
                    snapshot_path: None,
                    content_source: Some("http"),
                    summary: None,
                },
            )
            .expect("更新 archived 状态失败"));

        let tasks = store.list_archived_tasks(10).expect("查询 archived 失败");
        assert_eq!(
            tasks,
            vec![ArchivedTaskRecord {
                task_id: created.task_id,
                article_id: created.article_id,
                normalized_url: "https://example.com/archived-list".to_string(),
                title: Some("Archived List Title".to_string()),
                summary: None,
                content_source: Some("http".to_string()),
                page_kind: Some("article".to_string()),
                output_path: Some("/tmp/archived-list.md".to_string()),
                updated_at: tasks[0].updated_at.clone(),
            }]
        );
    }

    #[test]
    fn concurrent_writes_do_not_panic_on_busy() {
        let db_path = temp_db_path();
        let db_path = Arc::new(db_path);
        let threads: Vec<_> = (0..4)
            .map(|tid| {
                let path = Arc::clone(&db_path);
                thread::spawn(move || {
                    let mut store =
                        TaskStore::open(&*path).expect("并发线程初始化 task store 失败");
                    for i in 0..10 {
                        let msg_id = format!("msg-t{tid}-i{i}");
                        store
                            .record_inbound_message(&msg_id, "user-a", "hello")
                            .expect("并发写入不应 panic 或返回 BUSY");
                    }
                })
            })
            .collect();

        for t in threads {
            t.join().expect("并发线程不应 panic");
        }

        let conn = Connection::open(&*db_path).expect("验证读取失败");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM message_dedup", [], |row| row.get(0))
            .expect("计数失败");
        assert_eq!(count, 40, "40 条独立消息应全部写入");
    }

    #[test]
    fn context_token_can_be_persisted_and_loaded() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        store
            .upsert_context_token("user-a", "ctx-1")
            .expect("写入 token 失败");
        assert_eq!(
            store.get_context_token("user-a").expect("读取 token 失败"),
            Some("ctx-1".to_string())
        );

        store
            .upsert_context_token("user-a", "ctx-2")
            .expect("更新 token 失败");
        assert_eq!(
            store.get_context_token("user-a").expect("读取 token 失败"),
            Some("ctx-2".to_string())
        );
    }

    #[test]
    fn session_state_can_be_persisted_listed_and_deleted() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        store
            .upsert_session_state(
                "user-a",
                "hello\nworld",
                &["msg-1".to_string(), "msg-2".to_string()],
            )
            .expect("写入 session_state 失败");

        let sessions = store
            .list_session_states()
            .expect("查询 session_state 失败");
        assert_eq!(
            sessions.len(),
            1,
            "应只有一条 session_state，实际: {:?}",
            sessions
        );
        assert_eq!(
            sessions[0],
            StoredSessionRecord {
                user_id: "user-a".to_string(),
                merged_text: "hello\nworld".to_string(),
                message_ids: vec!["msg-1".to_string(), "msg-2".to_string()],
                updated_at: sessions[0].updated_at.clone(),
            }
        );

        store
            .delete_session_state("user-a")
            .expect("删除 session_state 失败");
        assert!(store
            .list_session_states()
            .expect("查询 session_state 失败")
            .is_empty());
    }

    #[test]
    fn cleanup_expired_user_session_states_cleans_both_tables() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        // 插入一条正常 session_state
        store
            .upsert_session_state("user-old", "旧会话", &["msg-old-1".to_string()])
            .expect("写入失败");
        // 插入一条旧 user_session_state（直接写旧 updated_at）
        store
            .upsert_user_session_state(&crate::task_store::UserSessionStateRecord {
                user_id: "user-old".to_string(),
                last_user_intent: Some("旧意图".to_string()),
                current_task: None,
                next_step: None,
                blocked_reason: None,
                goal: None,
                current_subtask: None,
                constraints_json: None,
                confirmed_facts_json: None,
                done_items_json: None,
                open_questions_json: None,
                updated_at: "2000-01-01T00:00:00Z".to_string(),
            })
            .expect("写入 v2 state 失败");

        // ttl=0 时， cutoff = now，旧记录应被清理
        let cleaned = store
            .cleanup_expired_user_session_states(0)
            .expect("清理失败");
        assert!(cleaned > 0, "应清理至少一条过期记录");

        // 两条表都应为空
        assert!(store.list_session_states().expect("查询失败").is_empty());
        assert!(store
            .load_user_session_state("user-old")
            .expect("加载失败")
            .is_none());
    }

    #[test]
    fn user_memory_can_be_added_and_listed() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let created = store
            .add_user_memory("user-a", "我更喜欢短摘要")
            .expect("写入 user_memory 失败");
        let memories = store
            .list_user_memories("user-a", 10)
            .expect("查询 user_memory 失败");

        assert_eq!(
            memories,
            vec![UserMemoryRecord {
                id: created.id,
                user_id: "user-a".to_string(),
                content: "我更喜欢短摘要".to_string(),
                memory_type: MemoryType::Explicit,
                status: "active".to_string(),
                priority: 100,
                last_used_at: None,
                use_count: 0,
                retrieved_count: 0,
                injected_count: 0,
                useful: false,
                created_at: created.created_at,
                updated_at: created.updated_at,
            }]
        );
    }

    #[test]
    fn user_memory_dedup_check_works() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        store
            .add_user_memory("user-a", "偏好: 短摘要")
            .expect("写入 user_memory 失败");

        assert!(store
            .has_user_memory("user-a", "偏好: 短摘要")
            .expect("查询 user_memory 去重失败"));
        assert!(!store
            .has_user_memory("user-a", "主题: Rust")
            .expect("查询 user_memory 去重失败"));
    }

    #[test]
    fn govern_dedup_checks_all_active_memories_not_top_50() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        // 写入 50 条高优先级 explicit 记忆，把后续 auto 记忆挤出 top 50
        for i in 0..50 {
            store
                .add_user_memory("user-dedup-all", &format!("高优先级记忆 {i}"))
                .expect("写入失败");
        }
        // 写入 1 条低优先级 auto 记忆（priority=60 < 100）
        let mut ws1 = MemoryWriteState::default();
        let decision = store.govern_memory_write(
            "user-dedup-all",
            "将被提升的偏好",
            MemoryType::Auto,
            60,
            &mut ws1,
        );
        let auto_id = match decision {
            WriteDecision::Written(r) => r.id,
            _ => panic!("应写入 auto memory"),
        };

        // 再写入 50 条高优先级 explicit 记忆，确保 auto 记忆在检索时远在 50 名之后
        for i in 0..50 {
            store
                .add_user_memory("user-dedup-all", &format!("高优先级记忆 {i}"))
                .expect("写入失败");
        }

        // 现在用 explicit 写入同一内容；若 dedup 仅查 top 50 会误判为新写入，
        // 全量查应正确识别为重复并 promote。
        let mut ws2 = MemoryWriteState::default();
        let decision = store.govern_memory_write(
            "user-dedup-all",
            "将被提升的偏好",
            MemoryType::Explicit,
            100,
            &mut ws2,
        );
        match decision {
            WriteDecision::Promoted { id, .. } => assert_eq!(id, auto_id),
            other => panic!("应 Promote 原有 auto memory: {:?}", other),
        }

        // 验证只存在一条该内容的记忆
        let memories = store
            .list_user_memories("user-dedup-all", 200)
            .expect("查询失败");
        assert_eq!(
            memories
                .iter()
                .filter(|m| m.content == "将被提升的偏好")
                .count(),
            1
        );
    }

    #[test]
    fn user_memory_schema_has_new_fields() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let created = store
            .add_user_memory("user-a", "显式记忆")
            .expect("写入 user_memory 失败");
        assert_eq!(created.memory_type, MemoryType::Explicit);
        assert_eq!(created.status, "active");
        assert_eq!(created.priority, 100);
        assert!(created.last_used_at.is_none());
        assert_eq!(created.use_count, 0);
    }

    #[test]
    fn user_memory_typed_auto() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let created = store
            .add_user_memory_typed("user-a", "自动提炼主题", MemoryType::Auto, 60)
            .expect("写入 auto memory 失败");
        assert_eq!(created.memory_type, MemoryType::Auto);
        assert_eq!(created.priority, 60);
    }

    #[test]
    fn user_memory_migration_adds_columns() {
        // 模拟老库：手动建只有旧字段的表，然后重新 open 触发 migration
        let db_path = temp_db_path();
        {
            let conn = Connection::open(&db_path).expect("打开数据库失败");
            conn.execute(
                "CREATE TABLE user_memories (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, content TEXT NOT NULL, created_at DATETIME NOT NULL, updated_at DATETIME NOT NULL)",
                [],
            ).expect("建旧表失败");
            conn.execute(
                "INSERT INTO user_memories (id, user_id, content, created_at, updated_at) VALUES ('m1', 'user-x', '旧数据', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                [],
            ).expect("插入旧数据失败");
        }
        // 重新 open 触发 migration
        let store = TaskStore::open(&db_path).expect("migration 后打开失败");
        let memories = store.list_user_memories("user-x", 10).expect("查询失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type, MemoryType::Explicit); // DEFAULT 值
        assert_eq!(memories[0].status, "active");
        assert_eq!(memories[0].priority, 100);
        assert_eq!(memories[0].use_count, 0);
    }

    #[test]
    fn search_memories_sorts_by_priority_and_dedupes() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        // auto 记忆优先级低
        store
            .add_user_memory_typed("user-a", "自动偏好", MemoryType::Auto, 60)
            .expect("写入 auto 失败");
        // explicit 记忆优先级高
        store
            .add_user_memory("user-a", "显式偏好")
            .expect("写入 explicit 失败");
        // 重复内容（多空格版本，split_whitespace 后与"显式偏好"不同，但与"显式 偏好"相同）
        store
            .add_user_memory("user-a", "显式  偏好")
            .expect("写入重复失败");
        // 真正的重复内容（只有空格差异）
        store
            .add_user_memory("user-a", "显式 偏好")
            .expect("写入真重复失败");

        let results = store.search_user_memories("user-a", 15).expect("检索失败");
        // 去重由 SessionState 负责，task_store 只返回排序后的结果
        // 4 条：显式  偏好(explicit), 显式偏好(explicit), 自动偏好(auto), 显式 偏好(explicit)
        assert_eq!(results.len(), 4);
        assert_eq!(results[0].memory_type, MemoryType::Explicit);
    }

    #[test]
    fn search_memories_respects_limit() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        store
            .add_user_memory("user-a", "短记忆一")
            .expect("写入失败");
        store
            .add_user_memory("user-a", "短记忆二")
            .expect("写入失败");
        store
            .add_user_memory("user-a", "短记忆三")
            .expect("写入失败");

        // limit=2，只返回 2 条
        let results = store.search_user_memories("user-a", 2).expect("检索失败");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn explicit_memory_sorts_before_auto() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        // 先写 auto，再写 explicit
        store
            .add_user_memory_typed("user-a", "自动偏好", MemoryType::Auto, 60)
            .expect("写入 auto 失败");
        store
            .add_user_memory("user-a", "显式偏好")
            .expect("写入 explicit 失败");

        let results = store.search_user_memories("user-a", 15).expect("检索失败");
        assert_eq!(results.len(), 2);
        // explicit (priority=100) 应排在 auto (priority=60) 前面
        assert_eq!(results[0].memory_type, MemoryType::Explicit);
        assert_eq!(results[0].priority, 100);
        assert_eq!(results[1].memory_type, MemoryType::Auto);
        assert_eq!(results[1].priority, 60);
    }

    #[test]
    fn search_memories_returns_all_sorted() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let long_content: String = "很".repeat(200);
        store
            .add_user_memory("user-a", &long_content)
            .expect("写入失败");
        store.add_user_memory("user-a", "短记忆").expect("写入失败");

        // task_store 只负责检索排序，不做预算裁剪
        let results = store.search_user_memories("user-a", 15).expect("检索失败");
        assert_eq!(results.len(), 2);
        // 两条都返回，trim 由 SessionState 负责
    }

    #[test]
    fn suppress_memory_excludes_from_results() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let created = store
            .add_user_memory("user-a", "将被抑制")
            .expect("写入失败");
        store
            .suppress_memory("user-a", &created.id)
            .expect("抑制失败");

        // list 只返回 active
        let listed = store.list_user_memories("user-a", 10).expect("查询失败");
        assert!(listed.is_empty());

        // search 也排除 suppressed
        let searched = store.search_user_memories("user-a", 15).expect("检索失败");
        assert!(searched.is_empty());

        // has_user_memory 也排除 suppressed
        assert!(!store
            .has_user_memory("user-a", "将被抑制")
            .expect("查询失败"));
    }

    #[test]
    fn user_memory_isolation() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        store
            .add_user_memory("user-a", "A 的记忆")
            .expect("写入失败");
        store
            .add_user_memory("user-b", "B 的记忆")
            .expect("写入失败");

        let a_memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(a_memories.len(), 1);
        assert_eq!(a_memories[0].content, "A 的记忆");

        let b_memories = store.list_user_memories("user-b", 10).expect("查询失败");
        assert_eq!(b_memories.len(), 1);
        assert_eq!(b_memories[0].content, "B 的记忆");
    }

    #[test]
    fn suppress_memory_rejects_other_users_memory() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let created = store
            .add_user_memory("user-a", "A 的私有记忆")
            .expect("写入失败");

        let err = store
            .suppress_memory("user-b", &created.id)
            .expect_err("跨用户屏蔽应失败");
        assert!(err.to_string().contains("未找到该记忆"));

        let listed = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);
    }

    #[test]
    fn suppress_memory_rejects_unknown_id() {
        let db_path = temp_db_path();
        let store = TaskStore::open(&db_path).expect("初始化 task store 失败");

        let err = store
            .suppress_memory("user-a", "missing-memory-id")
            .expect_err("不存在的 memory id 应失败");
        assert!(err.to_string().contains("未找到该记忆"));
    }

    #[test]
    fn task_store_log_payload_keeps_contract_fields() {
        let payload = build_task_store_log_payload(
            "error",
            "task_status_changed",
            vec![
                ("task_id", json!("task-1")),
                ("status", json!("failed")),
                ("detail", Value::Null),
            ],
        );

        assert_eq!(payload["level"], "error");
        assert_eq!(payload["event"], "task_status_changed");
        assert_eq!(payload["task_id"], "task-1");
        assert_eq!(payload["status"], "failed");
        assert!(payload.get("ts").is_some());
        assert!(payload.get("detail").is_none());
    }

    #[test]
    fn summary_is_overwritten_on_rerun() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化 task store 失败");
        let created = store
            .record_link_submission("https://example.com/summary-rerun")
            .expect("写入链接失败");

        store
            .claim_task(&created.task_id, "test-worker", 300)
            .expect("claim 失败");
        store
            .mark_task_archived(
                &created.task_id,
                MarkTaskArchivedInput {
                    output_path: "/tmp/summary-rerun.md",
                    title: Some("Summary Rerun"),
                    page_kind: Some("article"),
                    snapshot_path: None,
                    content_source: Some("http"),
                    summary: Some("初始摘要"),
                },
            )
            .expect("首次 archived 失败");

        // Simulate retry: reset then re-archive with better summary
        let conn = Connection::open(&db_path).expect("打开数据库失败");
        conn.execute(
            "UPDATE tasks SET status = 'pending', output_path = NULL, worker_id = NULL, processing_started_at = NULL, lease_until = NULL WHERE id = ?1",
            [created.task_id.as_str()],
        )
        .expect("重置任务状态失败");
        drop(conn);

        store
            .claim_task(&created.task_id, "test-worker", 300)
            .expect("claim 失败");
        store
            .mark_task_archived(
                &created.task_id,
                MarkTaskArchivedInput {
                    output_path: "/tmp/summary-rerun-v2.md",
                    title: Some("Summary Rerun"),
                    page_kind: Some("article"),
                    snapshot_path: None,
                    content_source: Some("http"),
                    summary: Some("更精确的LLM摘要"),
                },
            )
            .expect("二次 archived 失败");

        let archived = store.list_archived_tasks(10).expect("查询失败");
        assert_eq!(archived[0].summary, Some("更精确的LLM摘要".to_string()));
    }

    // ——— Phase 3: Memory Write Governance 测试 ———

    #[test]
    fn govern_writes_new_explicit_memory() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "我喜欢短摘要", MemoryType::Explicit, 100, &mut ws);
        match decision {
            WriteDecision::Written(r) => {
                assert_eq!(r.memory_type, MemoryType::Explicit);
                assert_eq!(r.priority, 100);
            }
            _ => panic!("应写入: {:?}", decision),
        }
        assert_eq!(ws.written_count(), 1);
        assert_eq!(ws.skipped_count(), 0);
        assert_eq!(ws.candidate_count, 1);
    }

    #[test]
    fn govern_writes_new_auto_memory() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws);
        match decision {
            WriteDecision::Written(r) => {
                assert_eq!(r.memory_type, MemoryType::Auto);
                assert_eq!(r.priority, 60);
            }
            _ => panic!("应写入: {:?}", decision),
        }
    }

    #[test]
    fn govern_skips_empty_content() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "   ", MemoryType::Explicit, 100, &mut ws);
        match decision {
            WriteDecision::Skipped {
                reason: SkipReason::Empty,
                ..
            } => {}
            _ => panic!("应跳过空内容: {:?}", decision),
        }
        assert_eq!(ws.skipped_count(), 1);
    }

    #[test]
    fn govern_skips_too_long_content() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let long: String = "很".repeat(501);
        let mut ws = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", &long, MemoryType::Explicit, 100, &mut ws);
        match decision {
            WriteDecision::Skipped {
                reason: SkipReason::TooLong,
                ..
            } => {}
            _ => panic!("应跳过超长: {:?}", decision),
        }
    }

    #[test]
    fn govern_skips_duplicate_same_type() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws1 = MemoryWriteState::default();
        let _ = store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws1);
        let mut ws2 = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws2);
        match decision {
            WriteDecision::Skipped {
                reason: SkipReason::Duplicate,
                ..
            } => {}
            _ => panic!("应跳过重复: {:?}", decision),
        }
    }

    #[test]
    fn govern_auto_does_not_downgrade_explicit() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        // 先写 explicit
        let mut ws1 = MemoryWriteState::default();
        let _ = store.govern_memory_write(
            "user-a",
            "偏好: 短摘要",
            MemoryType::Explicit,
            100,
            &mut ws1,
        );
        // 再尝试 auto 同内容
        let mut ws2 = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws2);
        match decision {
            WriteDecision::Skipped {
                reason: SkipReason::LowerPriorityWouldDowngradeHigher,
                ..
            } => {}
            _ => panic!("auto 不应降级 explicit: {:?}", decision),
        }
        // 验证原有 explicit 未被改变
        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type, MemoryType::Explicit);
    }

    #[test]
    fn govern_explicit_promotes_auto() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        // 先写 auto
        let mut ws1 = MemoryWriteState::default();
        let _ = store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws1);
        // 再写 explicit 同内容
        let mut ws2 = MemoryWriteState::default();
        let decision = store.govern_memory_write(
            "user-a",
            "偏好: 短摘要",
            MemoryType::Explicit,
            100,
            &mut ws2,
        );
        match decision {
            WriteDecision::Promoted {
                reason:
                    PromoteReason::TypePromotesLower {
                        from: MemoryType::Explicit,
                        to: MemoryType::Auto,
                    },
                ..
            } => {}
            _ => panic!("explicit 应提升 auto: {:?}", decision),
        }
        // 验证已提升
        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type, MemoryType::Explicit);
        assert_eq!(memories[0].priority, 100);
    }

    #[test]
    fn govern_write_state_counters_accurate() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();

        // 3 个候选
        let _ = store.govern_memory_write("user-a", "记忆一", MemoryType::Explicit, 100, &mut ws);
        let _ = store.govern_memory_write("user-a", "", MemoryType::Explicit, 100, &mut ws); // empty → skip
        let _ = store.govern_memory_write("user-a", "记忆一", MemoryType::Auto, 60, &mut ws); // dup → skip

        assert_eq!(ws.candidate_count, 3);
        assert_eq!(ws.written_count(), 1);
        assert_eq!(ws.skipped_count(), 2);
    }

    #[test]
    fn govern_write_state_no_cross_user_leak() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws_a = MemoryWriteState::default();
        let _ = store.govern_memory_write("user-a", "偏好: 红", MemoryType::Auto, 60, &mut ws_a);
        // user-b 的相同内容不应受 user-a 影响
        let mut ws_b = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-b", "偏好: 红", MemoryType::Auto, 60, &mut ws_b);
        match decision {
            WriteDecision::Written(_) => {}
            _ => panic!("user-b 应能写入: {:?}", decision),
        }
        // 各自独立
        assert_eq!(ws_a.written_count(), 1);
        assert_eq!(ws_b.written_count(), 1);
    }

    // ——— Phase 4: Memory Feedback 测试 ———

    #[test]
    fn feedback_retrieved_updates_retrieved_count() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "测试记忆", MemoryType::Explicit, 100, &mut ws);
        let memory_id = match decision {
            WriteDecision::Written(r) => r.id,
            _ => panic!("应写入"),
        };
        let mut fb = MemoryFeedbackState::default();
        fb.record(&memory_id, FeedbackKind::Retrieved);
        store.apply_memory_feedback(&fb).expect("feedback 写回失败");
        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories[0].retrieved_count, 1);
    }

    #[test]
    fn feedback_injected_updates_injected_count() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "测试记忆", MemoryType::Explicit, 100, &mut ws);
        let memory_id = match decision {
            WriteDecision::Written(r) => r.id,
            _ => panic!("应写入"),
        };
        let mut fb = MemoryFeedbackState::default();
        fb.record(&memory_id, FeedbackKind::Injected);
        store.apply_memory_feedback(&fb).expect("feedback 写回失败");
        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories[0].injected_count, 1);
    }

    #[test]
    fn feedback_useful_updates_use_count_and_useful_and_last_used_at() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "测试记忆", MemoryType::Explicit, 100, &mut ws);
        let memory_id = match decision {
            WriteDecision::Written(r) => r.id,
            _ => panic!("应写入"),
        };
        assert!(store.list_user_memories("user-a", 10).expect("查询失败")[0]
            .last_used_at
            .is_none());
        let mut fb = MemoryFeedbackState::default();
        fb.record(&memory_id, FeedbackKind::Useful);
        store.apply_memory_feedback(&fb).expect("feedback 写回失败");
        let mem = &store.list_user_memories("user-a", 10).expect("查询失败")[0];
        assert_eq!(mem.use_count, 1);
        assert!(mem.useful);
        assert!(mem.last_used_at.is_some());
    }

    #[test]
    fn confirm_memory_useful_enforces_user_ownership() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "测试记忆", MemoryType::Explicit, 100, &mut ws);
        let memory_id = match decision {
            WriteDecision::Written(r) => r.id,
            _ => panic!("应写入"),
        };

        let err = store
            .confirm_memory_useful("user-b", &memory_id)
            .expect_err("应拒绝其他用户标记有用");
        assert!(err.to_string().contains("无权标记有用"));

        store
            .confirm_memory_useful("user-a", &memory_id)
            .expect("同用户应可标记有用");
        let mem = &store.list_user_memories("user-a", 10).expect("查询失败")[0];
        assert!(mem.useful);
        assert_eq!(mem.use_count, 1);
    }

    #[test]
    fn explicit_still_sorts_before_auto() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        // auto with high use_count
        let mut ws = MemoryWriteState::default();
        let auto_id =
            match store.govern_memory_write("user-a", "主题: Rust", MemoryType::Auto, 60, &mut ws)
            {
                WriteDecision::Written(r) => r.id,
                _ => panic!("应写入"),
            };
        // 给 auto 大量 feedback
        let mut fb = MemoryFeedbackState::default();
        for _ in 0..10 {
            fb.record(&auto_id, FeedbackKind::Useful);
        }
        store.apply_memory_feedback(&fb).expect("feedback 失败");
        // 写入 explicit
        let mut ws2 = MemoryWriteState::default();
        let _ =
            store.govern_memory_write("user-a", "显式偏好", MemoryType::Explicit, 100, &mut ws2);
        let results = store.search_user_memories("user-a", 15).expect("检索失败");
        // explicit 仍然排第一
        assert_eq!(results[0].memory_type, MemoryType::Explicit);
        assert_eq!(results[0].priority, 100);
        assert_eq!(results[1].memory_type, MemoryType::Auto);
    }

    #[test]
    fn useful_auto_sorts_before_non_useful_auto() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();
        let useful_id =
            match store.govern_memory_write("user-a", "主题: Rust", MemoryType::Auto, 60, &mut ws)
            {
                WriteDecision::Written(r) => r.id,
                _ => panic!("应写入"),
            };
        let mut ws2 = MemoryWriteState::default();
        let _ = store.govern_memory_write("user-a", "主题: Python", MemoryType::Auto, 60, &mut ws2);
        // 给第一个 useful feedback
        let mut fb = MemoryFeedbackState::default();
        fb.record(&useful_id, FeedbackKind::Useful);
        store.apply_memory_feedback(&fb).expect("feedback 失败");
        let results = store.search_user_memories("user-a", 15).expect("检索失败");
        assert!(results[0].useful);
        assert!(!results[1].useful);
        assert_eq!(results[0].content, "主题: Rust");
    }

    #[test]
    fn higher_use_count_sorts_first() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws1 = MemoryWriteState::default();
        let id_high =
            match store.govern_memory_write("user-a", "主题: Rust", MemoryType::Auto, 60, &mut ws1)
            {
                WriteDecision::Written(r) => r.id,
                _ => panic!("应写入"),
            };
        let mut ws2 = MemoryWriteState::default();
        let _ = store.govern_memory_write("user-a", "主题: Go", MemoryType::Auto, 60, &mut ws2);
        // 给 Rust 5 次 useful
        let mut fb = MemoryFeedbackState::default();
        for _ in 0..5 {
            fb.record(&id_high, FeedbackKind::Useful);
        }
        store.apply_memory_feedback(&fb).expect("feedback 失败");
        let results = store.search_user_memories("user-a", 15).expect("检索失败");
        assert_eq!(results[0].content, "主题: Rust");
        assert!(results[0].use_count > results[1].use_count);
    }

    #[test]
    fn last_used_at_affects_sorting() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws1 = MemoryWriteState::default();
        let id_old =
            match store.govern_memory_write("user-a", "旧记忆", MemoryType::Auto, 60, &mut ws1) {
                WriteDecision::Written(r) => r.id,
                _ => panic!("应写入"),
            };
        let mut ws2 = MemoryWriteState::default();
        let id_new =
            match store.govern_memory_write("user-a", "新记忆", MemoryType::Auto, 60, &mut ws2) {
                WriteDecision::Written(r) => r.id,
                _ => panic!("应写入"),
            };
        // 只给"新记忆" useful feedback → 更新 last_used_at
        let mut fb = MemoryFeedbackState::default();
        fb.record(&id_new, FeedbackKind::Useful);
        store.apply_memory_feedback(&fb).expect("feedback 失败");
        let results = store.search_user_memories("user-a", 15).expect("检索失败");
        // 新记忆（useful=true, use_count=1）排在旧记忆（useful=false）前面
        assert_eq!(results[0].content, "新记忆");
        // 旧记忆没有 last_used_at
        assert!(store
            .list_user_memories("user-a", 10)
            .expect("查询失败")
            .iter()
            .find(|m| m.id == id_old)
            .unwrap()
            .last_used_at
            .is_none());
    }

    #[test]
    fn sorting_is_deterministic() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        // 写 5 条相同 type/priority 的 auto 记忆
        for i in 0..5 {
            let mut ws = MemoryWriteState::default();
            let _ = store.govern_memory_write(
                "user-a",
                &format!("记忆 {}", i),
                MemoryType::Auto,
                60,
                &mut ws,
            );
        }
        // 多次检索，结果必须一致
        let r1 = store.search_user_memories("user-a", 15).expect("检索失败");
        let r2 = store.search_user_memories("user-a", 15).expect("检索失败");
        assert_eq!(r1.len(), r2.len());
        for (a, b) in r1.iter().zip(r2.iter()) {
            assert_eq!(a.id, b.id);
        }
    }

    #[test]
    fn feedback_state_is_single_source() {
        // 验证 feedback 统计来自 MemoryFeedbackState，不各自重复计算
        let mut fb = MemoryFeedbackState::default();
        fb.record("m1", FeedbackKind::Retrieved);
        fb.record("m1", FeedbackKind::Injected);
        fb.record("m1", FeedbackKind::Useful);
        fb.record("m2", FeedbackKind::Retrieved);
        assert_eq!(fb.retrieved_count("m1"), 1);
        assert_eq!(fb.injected_count("m1"), 1);
        assert_eq!(fb.useful_count("m1"), 1);
        assert_eq!(fb.retrieved_count("m2"), 1);
        assert_eq!(fb.injected_count("m2"), 0);
        assert!(fb.has_feedback());
    }

    // ——— Phase 4: 新 Memory 类型测试 ———

    #[test]
    fn user_preference_can_be_written_and_retrieved() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let created = store
            .add_user_memory_typed("user-a", "我喜欢短摘要", MemoryType::UserPreference, 80)
            .expect("写入 user_preference 失败");

        assert_eq!(created.memory_type, MemoryType::UserPreference);
        assert_eq!(created.priority, 80);

        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type, MemoryType::UserPreference);
    }

    #[test]
    fn project_fact_can_be_written_and_retrieved() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let created = store
            .add_user_memory_typed(
                "user-a",
                "AMClaw 使用 Rust 开发",
                MemoryType::ProjectFact,
                85,
            )
            .expect("写入 project_fact 失败");

        assert_eq!(created.memory_type, MemoryType::ProjectFact);
        assert_eq!(created.priority, 85);

        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type, MemoryType::ProjectFact);
    }

    #[test]
    fn lesson_can_be_written_and_retrieved() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let created = store
            .add_user_memory_typed(
                "user-a",
                "链接抓取失败时应提示用户手动补录",
                MemoryType::Lesson,
                75,
            )
            .expect("写入 lesson 失败");

        assert_eq!(created.memory_type, MemoryType::Lesson);
        assert_eq!(created.priority, 75);

        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type, MemoryType::Lesson);
    }

    #[test]
    fn new_memory_types_sort_by_priority() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        store
            .add_user_memory_typed("user-a", "lesson", MemoryType::Lesson, 75)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-a", "auto", MemoryType::Auto, 60)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-a", "explicit", MemoryType::Explicit, 100)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-a", "project_fact", MemoryType::ProjectFact, 85)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-a", "user_preference", MemoryType::UserPreference, 80)
            .expect("写入失败");

        let results = store.search_user_memories("user-a", 15).expect("检索失败");
        assert_eq!(results.len(), 5);
        // explicit(100) > project_fact(85) > user_preference(80) > lesson(75) > auto(60)
        assert_eq!(results[0].memory_type, MemoryType::Explicit);
        assert_eq!(results[1].memory_type, MemoryType::ProjectFact);
        assert_eq!(results[2].memory_type, MemoryType::UserPreference);
        assert_eq!(results[3].memory_type, MemoryType::Lesson);
        assert_eq!(results[4].memory_type, MemoryType::Auto);
    }

    #[test]
    fn govern_user_preference_promotes_auto() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let mut ws1 = MemoryWriteState::default();
        let _ = store.govern_memory_write("user-a", "偏好: 短摘要", MemoryType::Auto, 60, &mut ws1);

        let mut ws2 = MemoryWriteState::default();
        let decision = store.govern_memory_write(
            "user-a",
            "偏好: 短摘要",
            MemoryType::UserPreference,
            80,
            &mut ws2,
        );

        match decision {
            WriteDecision::Promoted {
                reason:
                    PromoteReason::TypePromotesLower {
                        from: MemoryType::UserPreference,
                        to: MemoryType::Auto,
                    },
                ..
            } => {}
            _ => panic!("user_preference 应提升 auto: {:?}", decision),
        }

        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].memory_type, MemoryType::UserPreference);
        assert_eq!(memories[0].priority, 80);
    }

    #[test]
    fn govern_project_fact_cannot_downgrade_explicit() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let mut ws1 = MemoryWriteState::default();
        let _ = store.govern_memory_write(
            "user-a",
            "约束: 不用 unsafe",
            MemoryType::Explicit,
            100,
            &mut ws1,
        );

        let mut ws2 = MemoryWriteState::default();
        let decision = store.govern_memory_write(
            "user-a",
            "约束: 不用 unsafe",
            MemoryType::ProjectFact,
            85,
            &mut ws2,
        );

        match decision {
            WriteDecision::Skipped {
                reason: SkipReason::LowerPriorityWouldDowngradeHigher,
                ..
            } => {}
            _ => panic!("project_fact 不应覆盖 explicit: {:?}", decision),
        }
    }

    #[test]
    fn govern_lesson_skips_duplicate_project_fact() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let mut ws1 = MemoryWriteState::default();
        let _ = store.govern_memory_write(
            "user-a",
            "经验: 先 cargo check",
            MemoryType::ProjectFact,
            85,
            &mut ws1,
        );

        let mut ws2 = MemoryWriteState::default();
        let decision = store.govern_memory_write(
            "user-a",
            "经验: 先 cargo check",
            MemoryType::Lesson,
            75,
            &mut ws2,
        );

        // project_fact(85) > lesson(75)，所以 lesson 不能覆盖 project_fact
        match decision {
            WriteDecision::Skipped {
                reason: SkipReason::LowerPriorityWouldDowngradeHigher,
                ..
            } => {}
            _ => panic!("lesson 不应覆盖 project_fact: {:?}", decision),
        }
    }

    #[test]
    fn govern_explicit_promotes_lesson() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let mut ws1 = MemoryWriteState::default();
        let _ = store.govern_memory_write("user-a", "重要信息", MemoryType::Lesson, 75, &mut ws1);

        let mut ws2 = MemoryWriteState::default();
        let decision =
            store.govern_memory_write("user-a", "重要信息", MemoryType::Explicit, 100, &mut ws2);

        match decision {
            WriteDecision::Promoted {
                reason:
                    PromoteReason::TypePromotesLower {
                        from: MemoryType::Explicit,
                        to: MemoryType::Lesson,
                    },
                ..
            } => {}
            _ => panic!("explicit 应提升 lesson: {:?}", decision),
        }
    }

    #[test]
    fn memory_type_label_prefixes_are_correct() {
        assert_eq!(MemoryType::Explicit.label_prefix(), "[记忆]");
        assert_eq!(MemoryType::Auto.label_prefix(), "[记忆]");
        assert_eq!(MemoryType::UserPreference.label_prefix(), "[偏好]");
        assert_eq!(MemoryType::ProjectFact.label_prefix(), "[项目]");
        assert_eq!(MemoryType::Lesson.label_prefix(), "[经验]");
    }

    #[test]
    fn memory_write_threshold_skips_noise() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");
        let mut ws = MemoryWriteState::default();

        // 过短内容被跳过
        let d1 =
            store.govern_memory_write("user-a", "好的", MemoryType::UserPreference, 80, &mut ws);
        assert!(
            matches!(
                d1,
                WriteDecision::Skipped {
                    reason: SkipReason::Noise,
                    ..
                }
            ),
            "黑名单短句应被跳过: {:?}",
            d1
        );

        // 另一个黑名单
        let d2 = store.govern_memory_write("user-a", "OK", MemoryType::UserPreference, 80, &mut ws);
        assert!(
            matches!(
                d2,
                WriteDecision::Skipped {
                    reason: SkipReason::Noise,
                    ..
                }
            ),
            "ok 应被跳过: {:?}",
            d2
        );

        // 少于 6 字符被跳过
        let d3 = store.govern_memory_write("user-a", "短", MemoryType::UserPreference, 80, &mut ws);
        assert!(
            matches!(
                d3,
                WriteDecision::Skipped {
                    reason: SkipReason::Noise,
                    ..
                }
            ),
            "过短内容应被跳过: {:?}",
            d3
        );

        // 正常内容可通过
        let d4 = store.govern_memory_write(
            "user-a",
            "我喜欢在晚上看技术文章",
            MemoryType::UserPreference,
            80,
            &mut ws,
        );
        assert!(
            matches!(d4, WriteDecision::Written(_)),
            "正常内容应写入: {:?}",
            d4
        );

        // 查询确认只写入了正常内容
        let memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].content, "我喜欢在晚上看技术文章");
    }

    #[test]
    fn memory_type_user_isolation() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        store
            .add_user_memory_typed("user-a", "A 的偏好", MemoryType::UserPreference, 80)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-a", "A 的项目事实", MemoryType::ProjectFact, 85)
            .expect("写入失败");
        store
            .add_user_memory_typed("user-b", "B 的经验", MemoryType::Lesson, 75)
            .expect("写入失败");

        let a_memories = store.list_user_memories("user-a", 10).expect("查询失败");
        assert_eq!(a_memories.len(), 2);
        assert!(a_memories.iter().all(|m| m.user_id == "user-a"));

        let b_memories = store.list_user_memories("user-b", 10).expect("查询失败");
        assert_eq!(b_memories.len(), 1);
        assert_eq!(b_memories[0].memory_type, MemoryType::Lesson);
        assert_eq!(b_memories[0].content, "B 的经验");
    }

    // ——— UserSessionState 测试 ———

    #[test]
    fn user_session_state_empty_load_returns_none() {
        let db_path = temp_db_path();
        let store = TaskStore::open(&db_path).expect("初始化失败");
        let result = store.load_user_session_state("user-a").expect("加载失败");
        assert!(result.is_none());
    }

    #[test]
    fn user_session_state_first_write_and_read_back() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let record = UserSessionStateRecord {
            user_id: "user-a".to_string(),
            last_user_intent: Some("查询任务状态".to_string()),
            current_task: Some("task-123".to_string()),
            next_step: Some("等待用户确认".to_string()),
            blocked_reason: None,
            updated_at: "2026-04-17T10:00:00Z".to_string(),
            ..Default::default()
        };
        store.upsert_user_session_state(&record).expect("写入失败");

        let loaded = store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在记录");
        assert_eq!(loaded.user_id, "user-a");
        assert_eq!(loaded.last_user_intent, Some("查询任务状态".to_string()));
        assert_eq!(loaded.current_task, Some("task-123".to_string()));
        assert_eq!(loaded.next_step, Some("等待用户确认".to_string()));
        assert_eq!(loaded.blocked_reason, None);
    }

    #[test]
    fn user_session_state_overwrite_updates_fields() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let first = UserSessionStateRecord {
            user_id: "user-a".to_string(),
            last_user_intent: Some("旧意图".to_string()),
            current_task: Some("task-1".to_string()),
            next_step: Some("步骤1".to_string()),
            blocked_reason: None,
            updated_at: "2026-04-17T10:00:00Z".to_string(),
            ..Default::default()
        };
        store.upsert_user_session_state(&first).expect("写入失败");

        let second = UserSessionStateRecord {
            user_id: "user-a".to_string(),
            last_user_intent: Some("新意图".to_string()),
            current_task: Some("task-2".to_string()),
            next_step: Some("步骤2".to_string()),
            blocked_reason: Some("等待人工输入".to_string()),
            updated_at: "2026-04-17T11:00:00Z".to_string(),
            ..Default::default()
        };
        store.upsert_user_session_state(&second).expect("更新失败");

        let loaded = store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在记录");
        assert_eq!(loaded.last_user_intent, Some("新意图".to_string()));
        assert_eq!(loaded.current_task, Some("task-2".to_string()));
        assert_eq!(loaded.next_step, Some("步骤2".to_string()));
        assert_eq!(loaded.blocked_reason, Some("等待人工输入".to_string()));
        assert_eq!(loaded.updated_at, "2026-04-17T11:00:00Z".to_string());
    }

    #[test]
    fn user_session_state_user_isolation() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let record_a = UserSessionStateRecord {
            user_id: "user-a".to_string(),
            last_user_intent: Some("A的意图".to_string()),
            current_task: None,
            next_step: None,
            blocked_reason: None,
            updated_at: "2026-04-17T10:00:00Z".to_string(),
            ..Default::default()
        };
        let record_b = UserSessionStateRecord {
            user_id: "user-b".to_string(),
            last_user_intent: Some("B的意图".to_string()),
            current_task: None,
            next_step: None,
            blocked_reason: None,
            updated_at: "2026-04-17T10:00:00Z".to_string(),
            ..Default::default()
        };
        store
            .upsert_user_session_state(&record_a)
            .expect("写入A失败");
        store
            .upsert_user_session_state(&record_b)
            .expect("写入B失败");

        let loaded_a = store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在");
        let loaded_b = store
            .load_user_session_state("user-b")
            .expect("加载失败")
            .expect("应存在");
        assert_eq!(loaded_a.last_user_intent, Some("A的意图".to_string()));
        assert_eq!(loaded_b.last_user_intent, Some("B的意图".to_string()));
    }

    #[test]
    fn user_session_state_clear_removes_record() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let record = UserSessionStateRecord {
            user_id: "user-a".to_string(),
            last_user_intent: Some("意图".to_string()),
            current_task: None,
            next_step: None,
            blocked_reason: None,
            updated_at: "2026-04-17T10:00:00Z".to_string(),
            ..Default::default()
        };
        store.upsert_user_session_state(&record).expect("写入失败");
        assert!(store.load_user_session_state("user-a").unwrap().is_some());

        store.clear_user_session_state("user-a").expect("清空失败");
        assert!(store.load_user_session_state("user-a").unwrap().is_none());
    }

    #[test]
    fn user_session_state_upsert_empty_user_id_fails() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let record = UserSessionStateRecord {
            user_id: "   ".to_string(),
            last_user_intent: None,
            current_task: None,
            next_step: None,
            blocked_reason: None,
            updated_at: "2026-04-17T10:00:00Z".to_string(),
            ..Default::default()
        };
        let err = store
            .upsert_user_session_state(&record)
            .expect_err("应失败");
        assert!(err.to_string().contains("user_id"));
    }

    #[test]
    fn user_session_state_all_optional_fields_can_be_none() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let record = UserSessionStateRecord {
            user_id: "user-a".to_string(),
            last_user_intent: None,
            current_task: None,
            next_step: None,
            blocked_reason: None,
            updated_at: "2026-04-17T10:00:00Z".to_string(),
            ..Default::default()
        };
        store.upsert_user_session_state(&record).expect("写入失败");

        let loaded = store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在");
        assert!(loaded.last_user_intent.is_none());
        assert!(loaded.current_task.is_none());
        assert!(loaded.next_step.is_none());
        assert!(loaded.blocked_reason.is_none());
    }

    #[test]
    fn user_session_state_survives_reopen() {
        let db_path = temp_db_path();
        {
            let mut store = TaskStore::open(&db_path).expect("初始化失败");
            let record = UserSessionStateRecord {
                user_id: "user-a".to_string(),
                last_user_intent: Some("持久化测试".to_string()),
                current_task: Some("task-xyz".to_string()),
                next_step: None,
                blocked_reason: Some("blocked".to_string()),
                updated_at: "2026-04-17T12:00:00Z".to_string(),
                ..Default::default()
            };
            store.upsert_user_session_state(&record).expect("写入失败");
        }

        let store = TaskStore::open(&db_path).expect("重新打开失败");
        let loaded = store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在");
        assert_eq!(loaded.last_user_intent, Some("持久化测试".to_string()));
        assert_eq!(loaded.current_task, Some("task-xyz".to_string()));
        assert_eq!(loaded.blocked_reason, Some("blocked".to_string()));
    }

    #[test]
    fn user_session_state_v2_fields_round_trip() {
        let db_path = temp_db_path();
        let mut store = TaskStore::open(&db_path).expect("初始化失败");

        let mut record = UserSessionStateRecord {
            user_id: "user-a".to_string(),
            last_user_intent: Some("测试意图".to_string()),
            current_task: Some("task-v2".to_string()),
            next_step: Some("下一步".to_string()),
            blocked_reason: None,
            goal: Some("完成目标".to_string()),
            current_subtask: Some("当前子任务".to_string()),
            constraints_json: Some(r#"["约束1","约束2"]"#.to_string()),
            confirmed_facts_json: Some(r#"["事实A","事实B"]"#.to_string()),
            done_items_json: Some(r#"["完成1"]"#.to_string()),
            open_questions_json: Some(r#"["问题1","问题2"]"#.to_string()),
            updated_at: "2026-04-17T10:00:00Z".to_string(),
        };
        store.upsert_user_session_state(&record).expect("写入失败");

        let loaded = store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在");
        assert_eq!(loaded.goal, Some("完成目标".to_string()));
        assert_eq!(loaded.current_subtask, Some("当前子任务".to_string()));
        assert_eq!(loaded.constraints(), vec!["约束1", "约束2"]);
        assert_eq!(loaded.confirmed_facts(), vec!["事实A", "事实B"]);
        assert_eq!(loaded.done_items(), vec!["完成1"]);
        assert_eq!(loaded.open_questions(), vec!["问题1", "问题2"]);
        assert_eq!(loaded.populated_slot_count(), 7);
        assert!(!loaded.is_v2_empty());

        // 测试 set_ 方法
        record.set_constraints(vec!["新约束".to_string()]);
        record.set_confirmed_facts(vec![]);
        record.set_done_items(vec!["完成A".to_string(), "完成B".to_string()]);
        record.set_open_questions(vec![]);
        store.upsert_user_session_state(&record).expect("更新失败");

        let loaded2 = store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在");
        assert_eq!(loaded2.constraints(), vec!["新约束"]);
        assert!(loaded2.confirmed_facts_json.is_none());
        assert_eq!(loaded2.done_items(), vec!["完成A", "完成B"]);
        assert!(loaded2.open_questions_json.is_none());
    }

    #[test]
    fn user_session_state_v2_migration_on_existing_db() {
        // 模拟旧 DB（无 v2 字段），重新打开应自动迁移
        let db_path = temp_db_path();
        {
            let conn = rusqlite::Connection::open(&db_path).expect("打开失败");
            conn.execute(
                "CREATE TABLE user_session_states (
                    user_id TEXT PRIMARY KEY,
                    last_user_intent TEXT,
                    current_task TEXT,
                    next_step TEXT,
                    blocked_reason TEXT,
                    updated_at DATETIME NOT NULL
                )",
                [],
            )
            .expect("建旧表失败");
            conn.execute(
                "INSERT INTO user_session_states (user_id, last_user_intent, updated_at)
                 VALUES ('user-a', '旧意图', '2026-04-01T00:00:00Z')",
                [],
            )
            .expect("插入旧数据失败");
        }

        let store = TaskStore::open(&db_path).expect("重新打开失败");
        let loaded = store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在");
        assert_eq!(loaded.last_user_intent, Some("旧意图".to_string()));
        assert_eq!(loaded.goal, None);
        assert_eq!(loaded.constraints_json, None);
        assert!(loaded.is_v2_empty());

        // 写入 v2 数据应成功
        let mut store = TaskStore::open(&db_path).expect("重新打开失败");
        let mut record = loaded.clone();
        record.goal = Some("新目标".to_string());
        record.set_constraints(vec!["约束".to_string()]);
        store
            .upsert_user_session_state(&record)
            .expect("v2 写入失败");

        let loaded2 = store
            .load_user_session_state("user-a")
            .expect("加载失败")
            .expect("应存在");
        assert_eq!(loaded2.goal, Some("新目标".to_string()));
        assert_eq!(loaded2.constraints(), vec!["约束"]);
    }
}
