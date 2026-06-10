use super::super::{is_private_host_with, is_private_url, TaskStore};
use super::temp_db_path;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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
    assert!(is_private_url("http://169.254.169.254/latest/meta-data/"));
    assert!(is_private_url("http://100.64.0.1/cgn"));
    assert!(is_private_url("http://0x7f000001/ping"));
    assert!(is_private_url("http://0177.0.0.1/ping"));
    assert!(is_private_url("http://[::1]/ping"));
    assert!(is_private_url("http://[fc00::1]/ping"));
    // 公网不应命中
    assert!(!is_private_url("https://example.com/page"));
    assert!(!is_private_url("https://1.1.1.1/dns"));
    assert!(!is_private_url("https://8.8.8.8/dns"));
}

#[test]
fn fc_fd_prefix_domain_names_are_not_falsely_blocked() {
    assert!(!is_private_url("https://fc-news.example.com/page"));
    assert!(!is_private_url("https://fdomain.example.com/page"));
}

#[test]
fn domain_resolving_to_private_ip_is_blocked() {
    assert!(is_private_host_with("demo.test", |host| {
        if host == "demo.test" {
            vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))]
        } else {
            vec![]
        }
    }));
    assert!(is_private_host_with("demo6.test", |host| {
        if host == "demo6.test" {
            vec![IpAddr::V6(Ipv6Addr::LOCALHOST)]
        } else {
            vec![]
        }
    }));
}

#[test]
fn domain_resolving_to_public_ip_is_allowed() {
    assert!(!is_private_host_with("public.test", |host| {
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
