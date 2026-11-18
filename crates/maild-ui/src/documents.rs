//! 文档存储与资源通道（ADR-0033 决策 4）：令牌绑一封信，协议处理器只认令牌。
//!
//! 这里没有 Tauri 事件循环，只有纯数据结构与纯函数（路由解析、导航裁决、响应构造），
//! 好让「一份文档拿不到别的信的字节」与「除了自己的 URL 什么导航都不放行」在单测里钉死。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tauri::http::{header, Response, StatusCode};
use tauri::Url;

use crate::html::{Link, LINK_PATH};

/// 文档与 part 走的自定义协议名（`brand.toml` 的 `uri_scheme`；须与 `tauri.conf.json` CSP 的
/// `frame-src` 一致）。
pub const SCHEME: &str = maild_brand::URI_SCHEME;
/// 同时保留的文档上限（LRU）。切信会主动释放，这只是兜底。
pub const MAX_DOCUMENTS: usize = 8;

/// 协议基址按平台不同（Tauri：macOS/Linux `scheme://localhost`，Windows `http://scheme.localhost`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolBase {
    /// 文档与 part URL 的前缀，不带尾斜杠。
    pub url_prefix: String,
    /// 写进 CSP `img-src` 的来源表达式。
    pub csp_source: String,
}

impl ProtocolBase {
    pub fn for_platform() -> Self {
        if cfg!(windows) {
            Self {
                url_prefix: format!("http://{SCHEME}.localhost"),
                csp_source: format!("http://{SCHEME}.localhost"),
            }
        } else {
            Self {
                url_prefix: format!("{SCHEME}://localhost"),
                csp_source: format!("{SCHEME}:"),
            }
        }
    }

    pub fn doc_url(&self, token: &str) -> String {
        format!("{}/doc/{token}", self.url_prefix)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentIdentity {
    pub account: String,
    pub folder: String,
    pub uid: String,
}

/// 一份准备好的文档。`html` 已重写；`parts` 是本封可经 part 通道取回的 `part_index → mime`。
#[derive(Clone, Debug)]
pub struct Document {
    pub identity: DocumentIdentity,
    pub html: String,
    pub csp: String,
    pub links: Vec<Link>,
    pub parts: HashMap<u32, String>,
    pub allow_remote: bool,
}

#[derive(Default)]
struct Inner {
    /// 旧 → 新。
    docs: Vec<(String, Arc<Document>)>,
}

/// 进程内文档表。令牌是 128 位随机数的十六进制。
#[derive(Default)]
pub struct DocumentStore {
    inner: Mutex<Inner>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 令牌由调用方先用 [`new_token`] 生成——文档里的 `cid:`/链接 URL 在插入前就要带上它。
    pub fn insert(&self, token: String, document: Document) {
        let mut inner = self.inner.lock().unwrap();
        inner.docs.retain(|(candidate, _)| candidate != &token);
        inner.docs.push((token, Arc::new(document)));
        while inner.docs.len() > MAX_DOCUMENTS {
            inner.docs.remove(0);
        }
    }

    pub fn get(&self, token: &str) -> Option<Arc<Document>> {
        let inner = self.inner.lock().unwrap();
        inner
            .docs
            .iter()
            .find(|(candidate, _)| candidate == token)
            .map(|(_, document)| Arc::clone(document))
    }

    pub fn release(&self, token: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.docs.len();
        inner.docs.retain(|(candidate, _)| candidate != token);
        inner.docs.len() != before
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().docs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub fn new_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("系统随机源不可用");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_token(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 协议请求的路由。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Doc(String),
    Part(String, u32),
    Unknown,
}

pub fn route(path: &str) -> Route {
    let mut segments = path.trim_start_matches('/').split('/');
    match (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) {
        (Some("doc"), Some(token), None, _) if valid_token(token) => Route::Doc(token.to_string()),
        (Some("part"), Some(token), Some(part), None) if valid_token(token) => {
            match part.parse::<u32>() {
                Ok(part) => Route::Part(token.to_string(), part),
                Err(_) => Route::Unknown,
            }
        }
        _ => Route::Unknown,
    }
}

/// `on_navigation` 的裁决（决策 2/6）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
    /// 邮件里的链接被点了：取消导航，翻译成事件。
    Link {
        token: String,
        index: usize,
    },
}

/// 放行的只有三类：应用自身的 URL（由调用方按 dev/embedded 判定）、`about:blank`、本协议的 `/doc/`。
/// 本协议的 `/link/<token>/<n>` 翻译成链接事件（并取消导航）；`/part/` 作为导航目标一律拒绝——
/// 它只该是子资源。其余（`http(s)`、`file:`、`javascript:`、未注册 scheme……）拒绝。
pub fn navigation_verdict(
    url: &Url,
    base: &ProtocolBase,
    app_allows: &dyn Fn(&Url) -> bool,
) -> Verdict {
    if app_allows(url) {
        return Verdict::Allow;
    }
    if url.scheme() == "about" {
        return Verdict::Allow;
    }
    let text = url.as_str();
    if let Some(rest) = text.strip_prefix(&format!("{}/doc/", base.url_prefix)) {
        let token = rest.split(['#', '?']).next().unwrap_or("");
        return if valid_token(token) {
            Verdict::Allow
        } else {
            Verdict::Deny
        };
    }
    if let Some(rest) = text.strip_prefix(&format!("{}/{LINK_PATH}/", base.url_prefix)) {
        let mut segments = rest.split(['#', '?']).next().unwrap_or("").split('/');
        if let (Some(token), Some(index), None) =
            (segments.next(), segments.next(), segments.next())
        {
            if valid_token(token) {
                if let Ok(index) = index.parse::<usize>() {
                    return Verdict::Link {
                        token: token.to_string(),
                        index,
                    };
                }
            }
        }
        return Verdict::Deny;
    }
    Verdict::Deny
}

pub fn document_response(document: &Document) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, document.csp.as_str())
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "no-store")
        .body(document.html.clone().into_bytes())
        .expect("静态头部合法")
}

pub fn part_response(mime_type: &str, bytes: Vec<u8>) -> Response<Vec<u8>> {
    let mime = if mime_type.is_empty() || mime_type.contains(['\r', '\n']) {
        "application/octet-stream"
    } else {
        mime_type
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(bytes)
        .expect("静态头部合法")
}

pub fn not_found() -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(b"not found".to_vec())
        .expect("静态头部合法")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(uid: &str) -> Document {
        Document {
            identity: DocumentIdentity {
                account: "a".into(),
                folder: "INBOX".into(),
                uid: uid.into(),
            },
            html: format!("<p>{uid}</p>"),
            csp: "default-src 'none'".into(),
            links: vec![],
            parts: HashMap::from([(2u32, "image/png".to_string())]),
            allow_remote: false,
        }
    }

    fn base() -> ProtocolBase {
        ProtocolBase {
            url_prefix: format!("{SCHEME}://localhost"),
            csp_source: format!("{SCHEME}:"),
        }
    }

    #[test]
    fn tokens_are_random_hex_and_scoped() {
        let store = DocumentStore::new();
        let (first, second) = (new_token(), new_token());
        store.insert(first.clone(), document("1"));
        store.insert(second.clone(), document("2"));
        assert_ne!(first, second);
        assert!(valid_token(&first));
        assert_eq!(store.get(&first).unwrap().identity.uid, "1");
        assert_eq!(store.get(&second).unwrap().identity.uid, "2");
        assert!(store.get("0000000000000000000000000000000000").is_none());
        assert!(store.release(&first));
        assert!(!store.release(&first), "重复释放是 no-op");
        assert!(store.get(&first).is_none(), "释放后拿不到");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn the_store_is_bounded_oldest_first() {
        let store = DocumentStore::new();
        let tokens: Vec<String> = (0..(MAX_DOCUMENTS + 2))
            .map(|i| {
                let token = new_token();
                store.insert(token.clone(), document(&i.to_string()));
                token
            })
            .collect();
        assert_eq!(store.len(), MAX_DOCUMENTS);
        assert!(store.get(&tokens[0]).is_none());
        assert!(store.get(&tokens[1]).is_none());
        assert!(store.get(&tokens[2]).is_some());
        assert!(store.get(tokens.last().unwrap()).is_some());
    }

    #[test]
    fn routes_only_accept_well_formed_tokens() {
        let token = new_token();
        assert_eq!(route(&format!("/doc/{token}")), Route::Doc(token.clone()));
        assert_eq!(
            route(&format!("/part/{token}/7")),
            Route::Part(token.clone(), 7)
        );
        for bad in [
            &format!("/link/{token}/0"),
            "/doc/short",
            "/doc/../../etc",
            &format!("/doc/{token}/extra"),
            &format!("/part/{token}"),
            &format!("/part/{token}/x"),
            &format!("/part/{token}/1/2"),
            "/other",
            "/",
            "",
        ] {
            assert_eq!(route(bad), Route::Unknown, "{bad}");
        }
    }

    #[test]
    fn navigation_allows_only_the_app_and_document_urls() {
        let base = base();
        let token = new_token();
        let app = |url: &Url| url.scheme() == "tauri" || url.host_str() == Some("127.0.0.1");
        let verdict = |text: &str| navigation_verdict(&Url::parse(text).unwrap(), &base, &app);
        assert_eq!(verdict("tauri://localhost/index.html"), Verdict::Allow);
        assert_eq!(verdict("http://127.0.0.1:1420/"), Verdict::Allow);
        assert_eq!(verdict("about:blank"), Verdict::Allow);
        assert_eq!(
            verdict(&format!("{SCHEME}://localhost/doc/{token}")),
            Verdict::Allow
        );
        assert_eq!(
            verdict(&format!("{SCHEME}://localhost/doc/{token}#section")),
            Verdict::Allow
        );
        assert_eq!(verdict(&format!("{SCHEME}://localhost/doc/short")), Verdict::Deny);
        assert_eq!(
            verdict(&format!("{SCHEME}://localhost/part/{token}/2")),
            Verdict::Deny,
            "part 不是导航目标"
        );
        assert_eq!(
            verdict(&format!("{SCHEME}://localhost/link/{token}/3")),
            Verdict::Link {
                token: token.clone(),
                index: 3
            }
        );
        assert_eq!(verdict(&format!("{SCHEME}://localhost/link/zzz/0")), Verdict::Deny);
        assert_eq!(
            verdict(&format!("{SCHEME}://localhost/link/{token}/1/2")),
            Verdict::Deny
        );
        // 未注册 scheme：WebKit 在 sandbox 框架里根本不会送到这里；就算送到也拒绝。
        assert_eq!(
            verdict(&format!("maild-link://localhost/{token}/3")),
            Verdict::Deny
        );
        assert_eq!(
            verdict(&format!("{SCHEME}://localhost/link/{token}/x")),
            Verdict::Deny
        );
        for denied in [
            "https://example.com/",
            "http://evil.example/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,hi",
            "ftp://x/",
            "tauri-x://localhost/",
        ] {
            assert_eq!(verdict(denied), Verdict::Deny, "{denied}");
        }
    }

    #[test]
    fn responses_carry_isolation_headers() {
        let doc = document("1");
        let response = document_response(&doc);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert_eq!(
            response.headers()[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'"
        );
        assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.body(), b"<p>1</p>");

        let part = part_response("image/png", vec![1, 2, 3]);
        assert_eq!(part.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(part.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(part.headers()[header::CONTENT_LENGTH], "3");
        let odd = part_response("image/png\r\nX-Injected: 1", vec![]);
        assert_eq!(
            odd.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert_eq!(not_found().status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn platform_base_matches_tauri_url_shapes() {
        let base = ProtocolBase::for_platform();
        if cfg!(windows) {
            assert_eq!(base.url_prefix, format!("http://{SCHEME}.localhost"));
        } else {
            assert_eq!(base.url_prefix, format!("{SCHEME}://localhost"));
            assert_eq!(base.csp_source, format!("{SCHEME}:"));
        }
        assert!(base.doc_url("t").ends_with("/doc/t"));
    }
}
