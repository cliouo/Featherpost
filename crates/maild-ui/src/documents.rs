//! 文档存储与资源通道（ADR-0033 决策 4）：令牌绑一封信，协议处理器只认令牌。
//!
//! 这里没有 Tauri 事件循环，只有纯数据结构与纯函数（路由解析、导航裁决、响应构造），
//! 好让「一份文档拿不到别的信的字节」与「除了自己的 URL 什么导航都不放行」在单测里钉死。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tauri::http::{header, Response, StatusCode};
use tauri::Url;

use crate::html::{Link, LINK_PATH, REMOTE_PATH};

/// 文档与 part 走的自定义协议名（`brand.toml` 的 `uri_scheme`；须与 `tauri.conf.json` CSP 的
/// `frame-src` 一致）。
pub const SCHEME: &str = maild_brand::URI_SCHEME;
/// 同时保留的文档上限（LRU）。切信会主动释放，这只是兜底。
pub const MAX_DOCUMENTS: usize = 8;

/// 文档展示视图（原文 / 双语对照 / 仅译文）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslateView {
    Original,
    Bilingual,
    Translated,
}

/// 解析后的 `/doc/<token>?...` 查询参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocQuery {
    pub view: TranslateView,
    pub rev: Option<u64>,
    pub dark: bool,
}

/// `/doc/<token>?...` 查询参数解析错误（所有非法/重复/缺 rev 均 fail-closed 返回 404）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocQueryError {
    DuplicateKey,
    InvalidTranslateValue,
    InvalidRev,
    MissingRev,
    UnexpectedRev,
    InvalidAppearanceValue,
}

/// 严格解析 `/doc/<token>` 查询参数：
/// - 重复的 `translate` / `rev` / `appearance` 键一律拒绝；
/// - `translate=bilingual|translated` 必须同时带合法 `rev >= 1`；
/// - 未带 `translate` 时不得携带 `rev`；
/// - `appearance` 仅接受 `light` 或 `dark`。
pub fn parse_doc_query(query: Option<&str>) -> Result<DocQuery, DocQueryError> {
    let Some(raw_query) = query.filter(|q| !q.is_empty()) else {
        return Ok(DocQuery {
            view: TranslateView::Original,
            rev: None,
            dark: false,
        });
    };

    let mut view: Option<TranslateView> = None;
    let mut rev: Option<u64> = None;
    let mut dark: Option<bool> = None;

    for pair in raw_query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, val) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "translate" => {
                if view.is_some() {
                    return Err(DocQueryError::DuplicateKey);
                }
                view = Some(match val {
                    "bilingual" => TranslateView::Bilingual,
                    "translated" => TranslateView::Translated,
                    _ => return Err(DocQueryError::InvalidTranslateValue),
                });
            }
            "rev" => {
                if rev.is_some() {
                    return Err(DocQueryError::DuplicateKey);
                }
                if val.is_empty() || !val.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(DocQueryError::InvalidRev);
                }
                let parsed = val.parse::<u64>().map_err(|_| DocQueryError::InvalidRev)?;
                if parsed == 0 {
                    return Err(DocQueryError::InvalidRev);
                }
                rev = Some(parsed);
            }
            "appearance" => {
                if dark.is_some() {
                    return Err(DocQueryError::DuplicateKey);
                }
                dark = Some(match val {
                    "dark" => true,
                    "light" => false,
                    _ => return Err(DocQueryError::InvalidAppearanceValue),
                });
            }
            _ => {}
        }
    }

    let resolved_view = view.unwrap_or(TranslateView::Original);
    match resolved_view {
        TranslateView::Original => {
            if rev.is_some() {
                return Err(DocQueryError::UnexpectedRev);
            }
        }
        TranslateView::Bilingual | TranslateView::Translated => {
            if rev.is_none() {
                return Err(DocQueryError::MissingRev);
            }
        }
    }

    Ok(DocQuery {
        view: resolved_view,
        rev,
        dark: dark.unwrap_or(false),
    })
}

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
            Self::windows()
        } else {
            Self::unix()
        }
    }

    /// macOS / Linux 自定义 scheme 基址。
    pub fn unix() -> Self {
        Self {
            url_prefix: format!("{SCHEME}://localhost"),
            csp_source: format!("{SCHEME}:"),
        }
    }

    /// Windows WebView2 HTTP 形式基址。
    pub fn windows() -> Self {
        Self {
            url_prefix: format!("http://{SCHEME}.localhost"),
            csp_source: format!("http://{SCHEME}.localhost"),
        }
    }

    pub fn doc_url(&self, token: &str) -> String {
        format!("{}/doc/{token}", self.url_prefix)
    }

    /// 本文档已改写的惰性链接前缀（`<url_prefix>/link/<token>/`）。
    pub fn link_prefix(&self, token: &str) -> String {
        format!("{}/{LINK_PATH}/{token}/", self.url_prefix)
    }

    /// 跨平台生成带视图/修订号/外观参数的文档 URL。
    pub fn translated_doc_url(
        &self,
        token: &str,
        view: TranslateView,
        rev: u64,
        dark: bool,
    ) -> String {
        let base = self.doc_url(token);
        let mut params = Vec::new();
        match view {
            TranslateView::Original => {}
            TranslateView::Bilingual => {
                params.push("translate=bilingual".to_string());
                params.push(format!("rev={rev}"));
            }
            TranslateView::Translated => {
                params.push("translate=translated".to_string());
                params.push(format!("rev={rev}"));
            }
        }
        if dark {
            params.push("appearance=dark".to_string());
        }
        if params.is_empty() {
            base
        } else {
            format!("{base}?{}", params.join("&"))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentIdentity {
    pub account: String,
    pub folder: String,
    pub uid: String,
}

/// 绑定在某份隔离文档上的已提交译文快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentTranslation {
    pub rev: u64,
    pub job_id: String,
    pub provider_id: String,
    pub provider_generation: u64,
    pub source_lang: String,
    pub target_lang: String,
    pub translated_subject: Option<String>,
    pub bilingual_html: String,
    pub translated_html: String,
}

/// 某份文档当前在途的翻译任务句柄，用于释放/淘汰/换 token 时立即取消。
#[derive(Clone, Debug)]
pub struct ActiveDocJob {
    pub job_id: String,
    pub cancel_flag: Arc<AtomicBool>,
    pub cancel_notify: Arc<tokio::sync::Notify>,
}

/// 一份准备好的文档。`html` 已重写；`parts` 是本封可经 part 通道取回的 `part_index → mime`；
/// `remote_urls` 是 `allow_remote = true` 时经 `/remote/<token>/<idx>` 通道抓取的远程图片白名单表。
#[derive(Clone, Debug)]
pub struct Document {
    pub identity: DocumentIdentity,
    pub subject: String,
    pub html: String,
    pub csp: String,
    pub links: Vec<Link>,
    pub parts: HashMap<u32, String>,
    pub remote_urls: Vec<String>,
    pub allow_remote: bool,
    pub translation: Arc<RwLock<Option<DocumentTranslation>>>,
    pub translation_rev: Arc<AtomicU64>,
    pub active_job: Arc<Mutex<Option<ActiveDocJob>>>,
}

impl Document {
    /// 登记本份文档的新翻译任务；若已有旧在途任务则先立即取消它。
    pub fn begin_translation_job(
        &self,
        job_id: String,
    ) -> (Arc<AtomicBool>, Arc<tokio::sync::Notify>, Option<String>) {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_notify = Arc::new(tokio::sync::Notify::new());
        let mut guard = self
            .active_job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let replaced_job_id = guard.take().map(|prev| {
            prev.cancel_flag.store(true, Ordering::SeqCst);
            prev.cancel_notify.notify_waiters();
            prev.job_id
        });
        *guard = Some(ActiveDocJob {
            job_id,
            cancel_flag: Arc::clone(&cancel_flag),
            cancel_notify: Arc::clone(&cancel_notify),
        });
        (cancel_flag, cancel_notify, replaced_job_id)
    }

    /// 立即取消本份文档的在途翻译任务（文档释放、LRU 淘汰或手动取消时调用）。
    pub fn cancel_active_translation(&self) -> Option<String> {
        let mut guard = self
            .active_job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.take().map(|active| {
            active.cancel_flag.store(true, Ordering::SeqCst);
            active.cancel_notify.notify_waiters();
            active.job_id
        })
    }

    /// 若在途任务仍为 `job_id`，则清除占位（失败或取消清理用）。
    pub fn clear_active_job_if(&self, job_id: &str) {
        let mut guard = self
            .active_job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.as_ref().is_some_and(|j| j.job_id == job_id) {
            *guard = None;
        }
    }

    /// 校验 `job_id` 仍为当前活跃且未取消任务后，原子递增 `rev` 并提交译文快照。
    #[allow(clippy::too_many_arguments)]
    pub fn commit_translation(
        &self,
        job_id: &str,
        provider_id: String,
        provider_generation: u64,
        source_lang: String,
        target_lang: String,
        translated_subject: Option<String>,
        bilingual_html: String,
        translated_html: String,
    ) -> Option<u64> {
        let mut active_guard = self
            .active_job
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let matches_active = active_guard
            .as_ref()
            .is_some_and(|j| j.job_id == job_id && !j.cancel_flag.load(Ordering::SeqCst));
        if !matches_active {
            return None;
        }
        let rev = self.translation_rev.fetch_add(1, Ordering::SeqCst) + 1;
        let snapshot = DocumentTranslation {
            rev,
            job_id: job_id.to_string(),
            provider_id,
            provider_generation,
            source_lang,
            target_lang,
            translated_subject,
            bilingual_html,
            translated_html,
        };
        let mut tr_guard = self
            .translation
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *tr_guard = Some(snapshot);
        *active_guard = None;
        Some(rev)
    }

    pub fn current_translation(&self) -> Option<DocumentTranslation> {
        self.translation
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
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
    /// 若同令牌被替换或触发 LRU 淘汰，自动取消被移除文档的在途翻译任务。
    pub fn insert(&self, token: String, document: Document) {
        let _ = self.insert_with_evicted(token, document);
    }

    /// 同 [`Self::insert`]，并返回因替换或 LRU 淘汰而被取消的在途翻译 `job_id` 列表。
    pub fn insert_with_evicted(&self, token: String, document: Document) -> Vec<String> {
        let mut evicted_docs = Vec::new();
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut idx = 0;
            while idx < inner.docs.len() {
                if inner.docs[idx].0 == token {
                    evicted_docs.push(inner.docs.remove(idx).1);
                } else {
                    idx += 1;
                }
            }
            inner.docs.push((token, Arc::new(document)));
            while inner.docs.len() > MAX_DOCUMENTS {
                evicted_docs.push(inner.docs.remove(0).1);
            }
        }
        evicted_docs
            .into_iter()
            .filter_map(|doc| doc.cancel_active_translation())
            .collect()
    }

    pub fn get(&self, token: &str) -> Option<Arc<Document>> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner
            .docs
            .iter()
            .find(|(candidate, _)| candidate == token)
            .map(|(_, document)| Arc::clone(document))
    }

    /// 校验该 `token` 仍在表中且指向同一份 `Document` 实例（防止放行/撤回远图换 token 后旧任务回写）。
    pub fn is_live_instance(&self, token: &str, document: &Arc<Document>) -> bool {
        self.get(token)
            .is_some_and(|live| Arc::ptr_eq(&live, document))
    }

    pub fn release(&self, token: &str) -> bool {
        self.release_and_cancel(token).0
    }

    /// 释放指定令牌，并同步取消其上的在途翻译任务；返回 `(是否找到并释放, 被取消的 job_id)`。
    pub fn release_and_cancel(&self, token: &str) -> (bool, Option<String>) {
        let removed = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner
                .docs
                .iter()
                .position(|(candidate, _)| candidate == token)
                .map(|pos| inner.docs.remove(pos).1)
        };
        match removed {
            Some(doc) => (true, doc.cancel_active_translation()),
            None => (false, None),
        }
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .docs
            .len()
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
    Remote(String, usize),
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
        (Some(seg), Some(token), Some(idx), None) if seg == REMOTE_PATH && valid_token(token) => {
            match idx.parse::<usize>() {
                Ok(idx) => Route::Remote(token.to_string(), idx),
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

pub fn document_response(document: &Document, dark: bool) -> Response<Vec<u8>> {
    build_html_document_response(&document.html, &document.csp, dark)
}

/// 根据 `/doc/<token>?translate=bilingual|translated&rev=<n>[&appearance=dark]` 构造响应。
///
/// - GET 永不触发翻译：仅返回已提交且 `rev` 精确匹配的译文快照；
/// - 重复键、非法参数、缺失/过期 `rev` 一律 fail-closed 返回 404；
/// - 返回的隔离头（CSP、`no-referrer`、`nosniff`、`no-store`）与原文视图完全一致。
pub fn document_response_for_query(
    document: &Document,
    query: Option<&str>,
) -> Response<Vec<u8>> {
    let Ok(parsed) = parse_doc_query(query) else {
        return not_found();
    };
    match parsed.view {
        TranslateView::Original => {
            build_html_document_response(&document.html, &document.csp, parsed.dark)
        }
        TranslateView::Bilingual => {
            let Some(requested_rev) = parsed.rev else {
                return not_found();
            };
            let tr = document.current_translation();
            match tr {
                Some(snapshot) if snapshot.rev == requested_rev => {
                    build_html_document_response(&snapshot.bilingual_html, &document.csp, parsed.dark)
                }
                _ => not_found(),
            }
        }
        TranslateView::Translated => {
            let Some(requested_rev) = parsed.rev else {
                return not_found();
            };
            let tr = document.current_translation();
            match tr {
                Some(snapshot) if snapshot.rev == requested_rev => {
                    build_html_document_response(&snapshot.translated_html, &document.csp, parsed.dark)
                }
                _ => not_found(),
            }
        }
    }
}

fn build_html_document_response(raw_html: &str, csp: &str, dark: bool) -> Response<Vec<u8>> {
    let html = if dark {
        crate::html::dark_reading_html(raw_html)
    } else {
        raw_html.to_string()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CONTENT_SECURITY_POLICY, csp)
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CACHE_CONTROL, "no-store")
        .body(html.into_bytes())
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

/// 远程图片响应（`Route::Remote`）：强制 `image/*` MIME、`X-Content-Type-Options: nosniff`、`Cache-Control: no-store`。
pub fn remote_image_response(content_type: &str, bytes: Vec<u8>) -> Response<Vec<u8>> {
    let valid_image_mime = !content_type.is_empty()
        && !content_type.contains(['\r', '\n'])
        && content_type
            .split(';')
            .next()
            .map(str::trim)
            .is_some_and(|media| media.to_ascii_lowercase().starts_with("image/"));
    if !valid_image_mime {
        return not_found();
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
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
            subject: format!("Subject {uid}"),
            html: format!("<p>{uid}</p>"),
            csp: "default-src 'none'".into(),
            links: vec![],
            parts: HashMap::from([(2u32, "image/png".to_string())]),
            remote_urls: vec!["https://cdn.example.test/a.png".into()],
            allow_remote: false,
            translation: Arc::new(RwLock::new(None)),
            translation_rev: Arc::new(AtomicU64::new(0)),
            active_job: Arc::new(Mutex::new(None)),
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
        assert_eq!(
            route(&format!("/remote/{token}/0")),
            Route::Remote(token.clone(), 0)
        );
        assert_eq!(
            route(&format!("/remote/{token}/255")),
            Route::Remote(token.clone(), 255)
        );
        for bad in [
            &format!("/link/{token}/0"),
            "/doc/short",
            "/doc/../../etc",
            &format!("/doc/{token}/extra"),
            &format!("/part/{token}"),
            &format!("/part/{token}/x"),
            &format!("/part/{token}/1/2"),
            &format!("/remote/{token}"),
            &format!("/remote/{token}/x"),
            &format!("/remote/{token}/-1"),
            &format!("/remote/{token}/1/2"),
            "/remote/short/0",
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
            verdict(&format!("{SCHEME}://localhost/remote/{token}/0")),
            Verdict::Deny,
            "remote 图片不是导航目标"
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
        let response = document_response(&doc, false);
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
        let dark = document_response(&doc, true);
        assert_eq!(
            dark.headers(),
            response.headers(),
            "appearance must not relax isolation headers"
        );
        assert!(String::from_utf8_lossy(dark.body()).contains("#1b1b23"));
        assert_eq!(doc.html, "<p>1</p>", "the cached original stays unchanged");

        let part = part_response("image/png", vec![1, 2, 3]);
        assert_eq!(part.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(part.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(part.headers()[header::CONTENT_LENGTH], "3");
        let odd = part_response("image/png\r\nX-Injected: 1", vec![]);
        assert_eq!(
            odd.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
        let remote_ok = remote_image_response("image/webp", vec![4, 5, 6]);
        assert_eq!(remote_ok.status(), StatusCode::OK);
        assert_eq!(remote_ok.headers()[header::CONTENT_TYPE], "image/webp");
        assert_eq!(remote_ok.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(remote_ok.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(remote_ok.headers()[header::CONTENT_LENGTH], "3");
        assert_eq!(
            remote_image_response("text/html", b"<script>".to_vec()).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            remote_image_response("image/png\r\nX-Bad: 1", vec![]).status(),
            StatusCode::NOT_FOUND
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
