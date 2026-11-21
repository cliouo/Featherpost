//! 邮件 HTML 的重写器（ADR-0033 决策 3）：在字节离开 Rust 之前做**映射与去势**，不做白名单净化。
//!
//! 安全命题由三层同时成立：iframe `sandbox`、文档级 CSP、以及本模块 + 导航否决。本模块保证的是
//! 「即使 CSP 没生效，重写后的文档也发不出远程请求、链接也不能把用户带去任何地方」；
//! 版式忠实由 `lol_html` 的性质保证——未触碰的字节原样输出，表格、`<style>`、废弃属性一律保留。
//!
//! 不依赖 Tauri，可在无窗口的单测里对着敌意 fixture 跑。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use lol_html::html_content::{ContentType, Element};
use lol_html::{element, text, RewriteStrSettings};
use regex::Regex;
use serde::Serialize;

/// 内核在超限 HTML 末尾追加的标记（`maild::mime::HTML_TRUNCATION_MARKER`，ADR-0025）。
/// 这里不能 link 内核 crate，逐字复制并以单测钉住。
pub const TRUNCATION_MARKER: &str = "<!-- maild: truncated -->";

/// 链接改写成的惰性路径（决策 6）：`<协议基址>/link/<token>/<n>`。走**已注册**的 `maild_brand::URI_SCHEME` 协议
/// 而不是另起一个 scheme——实测 WebKit 对 sandbox 框架里指向未注册 scheme 的导航静默丢弃，根本
/// 不到导航 delegate；同一协议的 `/doc/` 已证明能到，`/link/` 走同一条路，在 `on_navigation` 里被
/// 取消并翻译成事件，协议处理器本身对它只回 404。
pub const LINK_PATH: &str = "link";

/// 整个元素（含内容）删除的标签。`form` 单独处理：删标签保内容，版式常靠它排布。
const REMOVED_ELEMENTS: &[&str] = &[
    "script", "iframe", "frame", "frameset", "object", "embed", "applet", "link", "base", "meta",
    "input", "button", "select", "textarea", "video", "audio", "source", "track",
];

/// 指向资源的属性：按 `cid:` / 远程 / 其他分类处理。
const RESOURCE_ATTRIBUTES: &[&str] = &[
    "src",
    "background",
    "poster",
    "lowsrc",
    "dynsrc",
    "longdesc",
];

/// 无条件删除的属性：表单目标、srcdoc、ping、下载与弹窗目标、appcache。
const DROPPED_ATTRIBUTES: &[&str] = &[
    "action",
    "formaction",
    "srcdoc",
    "ping",
    "target",
    "download",
    "manifest",
    "rel",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Http,
    Https,
    Mailto,
    /// `javascript:`、`file:`、`data:`、`cid:`、相对地址……全部惰性，只供查看。
    Other,
}

/// 文档链接表的一项。`href` 是原文（去首尾空白），`host` 只供显示。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Link {
    pub href: String,
    pub kind: LinkKind,
    pub host: String,
}

pub struct PrepareInput<'a> {
    pub html: &'a str,
    /// 本文档的令牌；`cid:` 与链接都改写成带令牌的 URL。
    pub token: &'a str,
    /// 协议基址，如 `<URI_SCHEME>://localhost`（按平台，见 `documents::ProtocolBase`）。
    pub part_url_prefix: &'a str,
    /// 文档级 CSP 的协议来源，如 `<URI_SCHEME>:`。
    pub csp_source: &'a str,
    /// `content_id → part_index`，只含已落库的附件。
    pub cid_parts: &'a HashMap<String, u32>,
    pub allow_remote: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub html: String,
    pub csp: String,
    pub links: Vec<Link>,
    /// 原文里的远程引用数（不论放行与否）。
    pub remote_refs: usize,
    pub cid_resolved: usize,
    pub cid_missing: usize,
    /// 既不是 cid、不是远程、也不是 data 的资源引用（`file:`、相对路径、我们自己的协议……）。
    pub dropped_refs: usize,
    pub truncated: bool,
    pub allow_remote: bool,
}

/// 文档级 CSP（决策 2）。放行只多给图片 `https: http:`；样式表、字体、媒体永远没有外部来源。
pub fn document_csp(csp_source: &str, allow_remote: bool) -> String {
    let remote = if allow_remote { " https: http:" } else { "" };
    format!(
        "default-src 'none'; img-src {csp_source} data:{remote}; style-src 'unsafe-inline'; \
         font-src 'none'; media-src 'none'; frame-src 'none'; object-src 'none'; \
         form-action 'none'; base-uri 'none'"
    )
}

#[derive(Default)]
struct State {
    links: Vec<Link>,
    remote_refs: usize,
    cid_resolved: usize,
    cid_missing: usize,
    dropped_refs: usize,
    saw_head: bool,
}

enum Resource {
    Keep,
    Rewrite(String),
    Remove,
}

struct Rewriter<'a> {
    token: &'a str,
    part_url_prefix: &'a str,
    cid_parts: &'a HashMap<String, u32>,
    allow_remote: bool,
}

impl Rewriter<'_> {
    fn part_url(&self, part_index: u32) -> String {
        format!(
            "{}/part/{}/{}",
            self.part_url_prefix, self.token, part_index
        )
    }

    /// 资源引用的分类。`data:` 保留（CSP 只对图片放它）；`cid:` 查表；远程按放行决定；其余删除。
    fn resource(&self, state: &mut State, raw: &str) -> Resource {
        let value = raw.trim();
        match scheme_of(value) {
            Some("cid") => {
                let id = value[4..]
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>');
                let found = self
                    .cid_parts
                    .get(id)
                    .or_else(|| {
                        self.cid_parts
                            .iter()
                            .find(|(key, _)| key.eq_ignore_ascii_case(id))
                            .map(|(_, part)| part)
                    })
                    .copied();
                match found {
                    Some(part) => {
                        state.cid_resolved += 1;
                        Resource::Rewrite(self.part_url(part))
                    }
                    None => {
                        state.cid_missing += 1;
                        Resource::Remove
                    }
                }
            }
            Some("data") => Resource::Keep,
            Some("http") | Some("https") => {
                state.remote_refs += 1;
                if self.allow_remote {
                    Resource::Keep
                } else {
                    Resource::Remove
                }
            }
            None if value.starts_with("//") => {
                state.remote_refs += 1;
                if self.allow_remote {
                    Resource::Keep
                } else {
                    Resource::Remove
                }
            }
            _ => {
                state.dropped_refs += 1;
                Resource::Remove
            }
        }
    }

    /// CSS 里的 `url()` 与 `@import`：cid 映射、远程按放行、其余中和成空 data URL（不破坏声明语法）。
    fn rewrite_css(&self, state: &mut State, css: &str) -> String {
        static IMPORT: OnceLock<Regex> = OnceLock::new();
        static URL: OnceLock<Regex> = OnceLock::new();
        let import = IMPORT.get_or_init(|| Regex::new(r"(?is)@import\b[^;{]*;?").expect("regex"));
        let url = URL.get_or_init(|| {
            Regex::new(r#"(?is)url\(\s*(?:"([^"]*)"|'([^']*)'|([^"')\s]*))\s*\)"#).expect("regex")
        });
        let without_imports = import.replace_all(css, |_: &regex::Captures| {
            state.remote_refs += 1;
            String::new()
        });
        url.replace_all(&without_imports, |captures: &regex::Captures| {
            let raw = captures
                .get(1)
                .or_else(|| captures.get(2))
                .or_else(|| captures.get(3))
                .map(|m| m.as_str())
                .unwrap_or("");
            match self.resource(state, raw) {
                Resource::Keep => captures[0].to_string(),
                // 单引号：`style` 属性值由 lol_html 用双引号包裹并转义内部双引号，单引号原样输出。
                Resource::Rewrite(target) => format!("url('{target}')"),
                Resource::Remove => "url('data:,')".to_string(),
            }
        })
        .into_owned()
    }

    fn rewrite_link(&self, state: &mut State, el: &mut Element, raw: &str) {
        let value = raw.trim();
        if value.is_empty() || value.starts_with('#') {
            return;
        }
        let index = state.links.len();
        state.links.push(classify_href(value));
        let target = format!(
            "{}/{LINK_PATH}/{}/{index}",
            self.part_url_prefix, self.token
        );
        let _ = el.set_attribute("href", &target);
    }

    fn rewrite_element(&self, state: &mut State, el: &mut Element) {
        let tag = el.tag_name().to_ascii_lowercase();
        if REMOVED_ELEMENTS.contains(&tag.as_str()) {
            el.remove();
            return;
        }
        if tag == "form" {
            el.remove_and_keep_content();
            return;
        }
        let is_link = tag == "a" || tag == "area";
        let is_svg_ref = tag == "image" || tag == "use" || tag == "feimage";
        let attributes: Vec<(String, String)> = el
            .attributes()
            .iter()
            .map(|attribute| (attribute.name(), attribute.value()))
            .collect();
        for (name, value) in attributes {
            let lower = name.to_ascii_lowercase();
            if lower.starts_with("on") || DROPPED_ATTRIBUTES.contains(&lower.as_str()) {
                el.remove_attribute(&name);
                continue;
            }
            if is_link && lower == "href" {
                self.rewrite_link(state, el, &value);
                continue;
            }
            if is_svg_ref && (lower == "href" || lower == "xlink:href") {
                self.apply_resource(state, el, &name, &value);
                continue;
            }
            if RESOURCE_ATTRIBUTES.contains(&lower.as_str()) {
                self.apply_resource(state, el, &name, &value);
                continue;
            }
            if lower == "srcset" {
                state.remote_refs += 1;
                if !self.allow_remote {
                    el.remove_attribute(&name);
                }
                continue;
            }
            if lower == "style" {
                let rewritten = self.rewrite_css(state, &value);
                if rewritten != value {
                    let _ = el.set_attribute(&name, &rewritten);
                }
            }
        }
    }

    fn apply_resource(&self, state: &mut State, el: &mut Element, name: &str, value: &str) {
        match self.resource(state, value) {
            Resource::Keep => {}
            Resource::Rewrite(target) => {
                let _ = el.set_attribute(name, &target);
            }
            Resource::Remove => {
                el.remove_attribute(name);
                // 被拦的远程图片：把原地址挪到 data-* 保留证据，让放行后的对比与调试有依据；
                // 浏览器对 data-* 不发请求。
                if name.eq_ignore_ascii_case("src")
                    && matches!(scheme_of(value.trim()), Some("http") | Some("https"))
                {
                    let _ = el.set_attribute("data-maild-src", value.trim());
                }
            }
        }
    }
}

/// 取 URL 的 scheme（小写）。没有合法 scheme（相对地址、实体编码把冒号藏起来了……）返回 `None`。
pub fn scheme_of(value: &str) -> Option<&str> {
    let colon = value.find(':')?;
    let scheme = &value[..colon];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic()
        || !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    {
        return None;
    }
    // 只能在 slice 上做小写比较：返回原切片，由调用方 eq_ignore_ascii_case。
    Some(match scheme {
        s if s.eq_ignore_ascii_case("http") => "http",
        s if s.eq_ignore_ascii_case("https") => "https",
        s if s.eq_ignore_ascii_case("cid") => "cid",
        s if s.eq_ignore_ascii_case("data") => "data",
        s if s.eq_ignore_ascii_case("mailto") => "mailto",
        other => other,
    })
}

/// 属性值里的 HTML 实体解码（`&amp;` 等）。lol_html 给的是源码原文；链接要按浏览器看到的样子
/// 登记，否则 `?a=1&amp;b=2` 会带着 `&amp;` 交给系统浏览器。只认 URL 里会出现的几种实体，
/// 未知实体原样保留。
pub fn decode_entities(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(position) = rest.find('&') {
        out.push_str(&rest[..position]);
        let tail = &rest[position..];
        let Some(end) = tail[..tail.len().min(12)].find(';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            numeric if numeric.starts_with('#') => {
                let digits = &numeric[1..];
                let code = match digits.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => digits.parse::<u32>().ok(),
                };
                code.and_then(char::from_u32).filter(|ch| !ch.is_control())
            }
            _ => None,
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 链接分类（决策 6）。`host` 只供显示：`http(s)` 取主机名，`mailto` 取地址，其余取协议名。
pub fn classify_href(href: &str) -> Link {
    let decoded = decode_entities(href);
    let value = decoded.trim();
    let (kind, host) = match scheme_of(value) {
        Some("http") => (LinkKind::Http, host_of(value)),
        Some("https") => (LinkKind::Https, host_of(value)),
        Some("mailto") => (
            LinkKind::Mailto,
            value[7..]
                .split('?')
                .next()
                .unwrap_or("")
                .trim()
                .to_string(),
        ),
        Some(other) => (LinkKind::Other, other.to_ascii_lowercase()),
        None => (LinkKind::Other, "relative".into()),
    };
    Link {
        href: value.to_string(),
        kind,
        host,
    }
}

fn host_of(url: &str) -> String {
    let rest = match url.find("://") {
        Some(position) => &url[position + 3..],
        None => return String::new(),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or(authority);
    host.to_ascii_lowercase()
}

/// 重写一封邮件的 HTML。失败（极端畸形输入让重写器报错）时返回错误，由调用方回退纯文本。
pub fn prepare(input: PrepareInput<'_>) -> Result<Prepared, String> {
    let csp = document_csp(input.csp_source, input.allow_remote);
    let inject = format!(
        "<meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\
         <meta name=\"referrer\" content=\"no-referrer\">"
    );
    let truncated = input.html.trim_end().ends_with(TRUNCATION_MARKER);
    let rewriter = Rewriter {
        token: input.token,
        part_url_prefix: input.part_url_prefix,
        cid_parts: input.cid_parts,
        allow_remote: input.allow_remote,
    };
    let state = Rc::new(RefCell::new(State::default()));
    let style_buffer = Rc::new(RefCell::new(String::new()));

    let element_state = Rc::clone(&state);
    let inject_for_head = inject.clone();
    let style_state = Rc::clone(&state);
    let style_buffer_for_text = Rc::clone(&style_buffer);
    let rewriter_ref = &rewriter;

    let output = lol_html::rewrite_str(
        input.html,
        RewriteStrSettings {
            element_content_handlers: vec![
                element!("head", move |el| {
                    element_state.borrow_mut().saw_head = true;
                    // 插入的内容不会再被处理器处理，原样输出。
                    el.prepend(&inject_for_head, ContentType::Html);
                    Ok(())
                }),
                element!("*", {
                    let state = Rc::clone(&state);
                    move |el| {
                        rewriter_ref.rewrite_element(&mut state.borrow_mut(), el);
                        Ok(())
                    }
                }),
                text!("style", move |chunk| {
                    // `<style>` 内容可能被切成多块；攒齐再重写。原文类型是 raw text，
                    // 用 Html 输出避免把 `&`/`<` 转义成实体破坏 CSS。
                    style_buffer_for_text.borrow_mut().push_str(chunk.as_str());
                    if chunk.last_in_text_node() {
                        let css = std::mem::take(&mut *style_buffer_for_text.borrow_mut());
                        let rewritten =
                            rewriter_ref.rewrite_css(&mut style_state.borrow_mut(), &css);
                        chunk.replace(&rewritten, ContentType::Html);
                    } else {
                        chunk.remove();
                    }
                    Ok(())
                }),
            ],
            ..RewriteStrSettings::new()
        },
    )
    .map_err(|error| format!("HTML 重写失败: {error}"))?;

    let state = Rc::try_unwrap(state)
        .map(RefCell::into_inner)
        .unwrap_or_else(|shared| shared.borrow().clone_out());
    let html = if state.saw_head {
        output
    } else {
        format!("<head>{inject}</head>{output}")
    };
    Ok(Prepared {
        html,
        csp,
        links: state.links,
        remote_refs: state.remote_refs,
        cid_resolved: state.cid_resolved,
        cid_missing: state.cid_missing,
        dropped_refs: state.dropped_refs,
        truncated,
        allow_remote: input.allow_remote,
    })
}

impl State {
    fn clone_out(&self) -> State {
        State {
            links: self.links.clone(),
            remote_refs: self.remote_refs,
            cid_resolved: self.cid_resolved,
            cid_missing: self.cid_missing,
            dropped_refs: self.dropped_refs,
            saw_head: self.saw_head,
        }
    }
}

/// Optional reading palette for an already-rewritten document. Never invert image
/// pixels, change resource URLs, or alter the stored original / its capabilities.
pub fn dark_reading_html(html: &str) -> String {
    let adapted = lol_html::rewrite_str(html, RewriteStrSettings {
        element_content_handlers: vec![element!("*", |el| {
            let tag = el.tag_name().to_ascii_lowercase();
            if el.namespace_uri() != "http://www.w3.org/1999/xhtml"
                || ["img", "head", "style", "meta", "link"].contains(&tag.as_str()) {
                return Ok(());
            }
            let original = el.get_attribute("style").unwrap_or_default();
            let background = if tag == "html" || tag == "body" { "#1b1b23" }
                else if el.get_attribute("bgcolor").is_some() || original.to_ascii_lowercase().contains("background") { "#252530" }
                else { "transparent" };
            let foreground = if tag == "a" || tag == "area" { "#b1ccff" } else { "#e4e4ed" };
            let style = format!("{original};background-color:{background}!important;background-image:none!important;color:{foreground}!important;border-color:#454550!important;box-shadow:none!important;");
            let _ = el.set_attribute("style", &style);
            Ok(())
        })],
        ..RewriteStrSettings::new()
    }).unwrap_or_else(|_| html.to_owned());
    // Fragments may have no explicit html/body tags for the element handler.
    format!("<style>html:root,html:root body{{background-color:#1b1b23!important;color:#e4e4ed!important;color-scheme:dark}}html:root{{min-height:100%}}</style>{adapted}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";
    /// 协议基址与 CSP 来源都从品牌 scheme 推出，测试对任意 `brand.toml` 成立。
    fn prefix() -> String {
        format!("{}://localhost", maild_brand::URI_SCHEME)
    }

    fn csp_source() -> String {
        format!("{}:", maild_brand::URI_SCHEME)
    }

    fn cid_parts() -> HashMap<String, u32> {
        HashMap::from([
            ("logo@maild".to_string(), 3u32),
            ("Hero@Maild".to_string(), 4u32),
        ])
    }

    fn run(html: &str, allow_remote: bool) -> Prepared {
        let parts = cid_parts();
        let (prefix, csp_source) = (prefix(), csp_source());
        prepare(PrepareInput {
            html,
            token: TOKEN,
            part_url_prefix: &prefix,
            csp_source: &csp_source,
            cid_parts: &parts,
            allow_remote,
        })
        .expect("重写成功")
    }

    #[test]
    fn dark_reading_overrides_authored_white_and_preserves_resources_and_links() {
        let prepared = run(
            r##"<body bgcolor="#ffffff" style="background:white!important;color:black">
            <table bgcolor="#fff"><tr><td style="color:#000!important">Readable</td></tr></table>
            <a href="https://example.com">Link</a><img src="cid:logo@maild" style="width:80px">
            <img src="https://remote.example/pixel.png"><script>alert(1)</script></body>"##,
            false,
        );
        let dark = dark_reading_html(&prepared.html);
        assert!(dark.contains("background-color:#1b1b23!important"));
        assert!(dark.contains("color:#e4e4ed!important"));
        assert!(dark.contains("color:#b1ccff!important"));
        assert!(dark.contains(&format!(
            "src=\"{}/part/{TOKEN}/3\" style=\"width:80px\"",
            prefix()
        )));
        assert!(dark.contains(&format!("href=\"{}/link/{TOKEN}/0\"", prefix())));
        assert!(!dark.contains(" src=\"https://"));
        assert!(!dark.contains("<script"));
        assert!(
            !dark.contains("filter:"),
            "image pixels must not be inverted"
        );
        assert_eq!(prepared.remote_refs, 1);
        assert!(!prepared.allow_remote);
    }

    #[test]
    fn dark_reading_handles_fragments_and_already_dark_mail_without_color_inversion() {
        for source in [
            "<p>Fragment</p>",
            "<body style=\"background:black;color:white\"><p>Dark mail</p></body>",
        ] {
            let prepared = run(source, false);
            let dark = dark_reading_html(&prepared.html);
            assert!(dark.contains("html:root"));
            assert!(dark.contains("background-color:#1b1b23!important"));
            assert!(dark.contains("color:#e4e4ed!important"));
            assert!(!dark.contains("invert("));
        }
    }

    /// 敌意样本；`@SCHEME@` 由 [`hostile`] 换成品牌协议名。
    const HOSTILE_TEMPLATE: &str = r##"<!DOCTYPE html><html><head><meta http-equiv="refresh" content="0;url=https://evil.example/">
<meta charset="gbk"><base href="https://evil.example/"><link rel="stylesheet" href="https://evil.example/a.css">
<script>alert(1)</script><style>@import url(https://evil.example/b.css); .x{background:url("cid:logo@maild")} .y{background:url(https://evil.example/t.gif)} .z{color:red}</style></head>
<body onload="alert(2)" background="https://evil.example/bg.png">
<p style="background-image:url('https://evil.example/p.png'); color: blue">你好，世界 &amp; 报表</p>
<table cellpadding="4" bgcolor="#eeeeee"><tr><td width="200"><font color="red">季度</font></td></tr></table>
<img src="cid:logo@maild" width="80"><img src="CID:<hero@maild>"><img src="cid:absent@maild" alt="缺">
<img src="https://evil.example/pixel.gif" srcset="https://evil.example/2x.gif 2x" onerror="alert(3)">
<img src="data:image/gif;base64,R0lGODlh" alt="inline">
<img src="file:///etc/passwd"><img src="relative/img.png"><img src="@SCHEME@://localhost/doc/other">
<svg><script>alert(4)</script><image href="cid:logo@maild"/><use xlink:href="https://evil.example/s.svg#i"/></svg>
<form action="https://evil.example/post" method="post"><input type="text" name="q"><button>go</button><span>表单里的文字</span></form>
<iframe src="https://evil.example/"></iframe><object data="x.swf"></object><embed src="x.swf">
<video src="https://evil.example/v.mp4" poster="https://evil.example/p.jpg"></video>
<a href="https://Example.COM/path?x=1&amp;y=2" target="_blank" ping="https://evil.example/ping" download>链接</a>
<a href="javascript:alert(5)">js</a><a href="mailto:Someone@Example.com?subject=hi">写信</a>
<a href="#section">锚点</a><a href="file:///etc/hosts">文件</a><a href="relative/page.html">相对</a>
<a href="maild-link://localhost/zzz/0">伪造</a><a href="@SCHEME@://localhost/link/zzz/1">伪造二</a>
</body></html>"##;

    fn hostile() -> String {
        HOSTILE_TEMPLATE.replace("@SCHEME@", maild_brand::URI_SCHEME)
    }

    #[test]
    fn scripts_forms_frames_and_meta_are_gone_but_layout_survives() {
        let out = run(&hostile(), false);
        let html = out.html.to_ascii_lowercase();
        let own_doc = format!("{}/doc/other", prefix()).to_ascii_lowercase();
        for forbidden in [
            "<script",
            "alert(",
            "<iframe",
            "<object",
            "<embed",
            "<link",
            "<base",
            "http-equiv=\"refresh\"",
            "charset=\"gbk\"",
            "<form",
            "<input",
            "<button",
            "<video",
            "onload=",
            "onerror=",
            "action=",
            "ping=",
            "target=",
            "download",
            " src=\"https://",
            "href=\"https://",
            "background=\"https://",
            "url(https://",
            "srcset=",
            "poster=",
            "file:///",
            "relative/img.png",
            &own_doc,
            "maild-link://localhost/zzz",
            "@import",
        ] {
            assert!(
                !html.contains(forbidden),
                "输出里不该有 {forbidden:?}:\n{}",
                out.html
            );
        }
        // 远程地址只允许以惰性证据 `data-maild-src` 的形式存在（浏览器对 data-* 不发请求）。
        assert_eq!(
            html.matches("evil.example").count(),
            html.matches("data-maild-src=\"").count(),
            "远程地址只能出现在 data-maild-src 里:\n{}",
            out.html
        );
        // 版式与文本逐字保留。
        for kept in [
            "<table cellpadding=\"4\" bgcolor=\"#eeeeee\">",
            "<td width=\"200\">",
            "<font color=\"red\">季度</font>",
            "你好，世界 &amp; 报表",
            "color: blue",
            ".z{color:red}",
            "<span>表单里的文字</span>",
            "<img src=\"data:image/gif;base64,R0lGODlh\" alt=\"inline\">",
        ] {
            assert!(
                out.html.contains(kept),
                "输出里应保留 {kept:?}:\n{}",
                out.html
            );
        }
        // 我们只注入三条 meta，不注入字体或版式。
        assert!(out.html.contains("<meta charset=\"utf-8\">"));
        assert!(out
            .html
            .contains("<meta http-equiv=\"Content-Security-Policy\""));
        assert!(out
            .html
            .contains("<meta name=\"referrer\" content=\"no-referrer\">"));
        assert!(!out.html.contains("font-family"), "不注入字体");
        assert!(!out.truncated);
    }

    #[test]
    fn cid_references_map_to_token_scoped_part_urls() {
        let out = run(&hostile(), false);
        let prefix = prefix();
        let logo = format!("{prefix}/part/{TOKEN}/3");
        let hero = format!("{prefix}/part/{TOKEN}/4");
        assert!(out
            .html
            .contains(&format!("<img src=\"{logo}\" width=\"80\">")));
        // 大小写与尖括号都容忍。
        assert!(out.html.contains(&format!("<img src=\"{hero}\">")));
        // svg image 与 CSS url(cid:) 同样映射（lol_html 重排属性后自闭合前多一个空格，不纠结）。
        assert!(out.html.contains(&format!("<image href=\"{logo}\"")));
        assert!(out
            .html
            .contains(&format!(".x{{background:url('{logo}')}}")));
        // 找不到的 cid：去掉 src，保留 alt。
        assert!(out.html.contains("<img alt=\"缺\">"));
        assert_eq!(out.cid_resolved, 4);
        assert_eq!(out.cid_missing, 1);
    }

    #[test]
    fn remote_references_are_counted_and_neutralised_when_blocked() {
        let blocked = run(&hostile(), false);
        // body background、p style、img src、srcset、svg use、@import、style .y = 7。
        // `<video>` 整个元素先被删，它的 src/poster 不再计入——删除的元素不需要再统计引用。
        assert_eq!(blocked.remote_refs, 7);
        // 带前导空格：`data-maild-src="https://…"` 是允许存在的惰性证据。
        assert!(
            !blocked.html.contains(" src=\"https://"),
            "{}",
            blocked.html
        );
        assert!(!blocked.html.contains("srcset="));
        assert!(blocked
            .html
            .contains("data-maild-src=\"https://evil.example/pixel.gif\""));
        assert!(blocked.html.contains(".y{background:url('data:,')}"));
        assert!(blocked.html.contains("background-image:url('data:,')"));
        assert!(blocked
            .csp
            .contains(&format!("img-src {} data:;", csp_source())));
        assert!(!blocked.allow_remote);

        let allowed = run(&hostile(), true);
        assert!(allowed.html.contains("<img src=\"https://evil.example/pixel.gif\" srcset=\"https://evil.example/2x.gif 2x\">"));
        assert!(allowed
            .html
            .contains(".y{background:url(https://evil.example/t.gif)}"));
        assert!(allowed
            .html
            .contains("background=\"https://evil.example/bg.png\""));
        assert!(allowed
            .csp
            .contains(&format!("img-src {} data: https: http:;", csp_source())));
        // 放行也不放样式表、字体、媒体、脚本。
        assert!(!allowed.html.to_ascii_lowercase().contains("<link"));
        assert!(!allowed.html.contains("@import"));
        assert!(!allowed.html.to_ascii_lowercase().contains("<video"));
        assert!(!allowed.html.contains("<script"));
        assert!(allowed.csp.contains("font-src 'none'"));
        assert!(allowed.csp.contains("style-src 'unsafe-inline';"));
        assert_eq!(allowed.remote_refs, blocked.remote_refs);
    }

    #[test]
    fn links_are_indexed_classified_and_made_inert() {
        let out = run(&hostile(), false);
        let prefix = prefix();
        let forged = format!("{prefix}/link/zzz/1");
        let expect = [
            (
                "https://Example.COM/path?x=1&y=2",
                LinkKind::Https,
                "example.com",
            ),
            ("javascript:alert(5)", LinkKind::Other, "javascript"),
            (
                "mailto:Someone@Example.com?subject=hi",
                LinkKind::Mailto,
                "Someone@Example.com",
            ),
            ("file:///etc/hosts", LinkKind::Other, "file"),
            ("relative/page.html", LinkKind::Other, "relative"),
            (
                "maild-link://localhost/zzz/0",
                LinkKind::Other,
                "maild-link",
            ),
            (forged.as_str(), LinkKind::Other, maild_brand::URI_SCHEME),
        ];
        assert_eq!(out.links.len(), expect.len(), "{:?}", out.links);
        for (index, (href, kind, host)) in expect.iter().enumerate() {
            assert_eq!(out.links[index].href, *href);
            assert_eq!(out.links[index].kind, *kind);
            assert_eq!(out.links[index].host, *host);
            assert!(
                out.html
                    .contains(&format!("href=\"{prefix}/link/{TOKEN}/{index}\"")),
                "第 {index} 个链接应改写成惰性 URL"
            );
        }
        // 片段链接原样保留（文档内跳转）。
        assert!(out.html.contains("<a href=\"#section\">锚点</a>"));
        assert!(!out.html.contains("javascript:"));
        // 原文里伪造的 /link/ 路径被当成普通 Other 链接重新编号，不可能直接命中别的序号。
        assert!(!out.html.contains("/link/zzz/"));
    }

    #[test]
    fn documents_without_head_get_one_and_truncation_is_detected() {
        let out = run("<p>只有一段</p>", false);
        assert!(out.html.starts_with("<head><meta charset=\"utf-8\">"));
        assert!(out.html.ends_with("<p>只有一段</p>"));
        assert_eq!(out.remote_refs, 0);
        assert_eq!(out.links.len(), 0);

        let truncated = run(
            &format!("<html><head></head><body>x</body></html>{TRUNCATION_MARKER}"),
            false,
        );
        assert!(truncated.truncated);
        assert_eq!(
            truncated.html.matches("<meta charset=\"utf-8\">").count(),
            1,
            "已有 head 时不重复注入"
        );
    }

    #[test]
    fn split_style_chunks_are_reassembled_before_rewriting() {
        // 8 KiB 的 CSS 让 lol_html 分多块回调；重写必须看到完整文本。
        let filler = ".f{color:#000}\n".repeat(600);
        let html = format!("<style>{filler}.k{{background:url(cid:logo@maild)}}</style><p>t</p>");
        let out = run(&html, false);
        let prefix = prefix();
        assert!(out
            .html
            .contains(&format!(".k{{background:url('{prefix}/part/{TOKEN}/3')}}")));
        assert_eq!(out.html.matches(".f{color:#000}").count(), 600);
    }

    #[test]
    fn scheme_detection_ignores_entity_smuggling_and_whitespace() {
        assert_eq!(scheme_of("HTTPS://a"), Some("https"));
        assert_eq!(scheme_of("javascript&colon;alert(1)"), None);
        assert_eq!(scheme_of("relative/path"), None);
        assert_eq!(scheme_of("//cdn.example/x"), None);
        assert_eq!(scheme_of("1http://a"), None);
        assert_eq!(
            classify_href("  https://user:pw@Host.Example:8443/p  ").host,
            "host.example:8443"
        );
        assert_eq!(classify_href("mailto:a@b.c").kind, LinkKind::Mailto);
        // 实体解码：`&amp;` 还原；藏在实体里的 `javascript&colon;` 仍然不是 http(s)，归 Other。
        assert_eq!(
            decode_entities("a?x=1&amp;y=&#50;&#x33;&quot;"),
            "a?x=1&y=23\""
        );
        assert_eq!(decode_entities("&unknown; & &amp"), "&unknown; & &amp");
        assert_eq!(
            classify_href("javascript&colon;alert(1)").kind,
            LinkKind::Other
        );
        assert_eq!(
            classify_href("&#106;avascript:alert(1)").kind,
            LinkKind::Other
        );
        assert_eq!(classify_href("data:text/html,hi").kind, LinkKind::Other);
    }

    #[test]
    fn truncation_marker_matches_the_kernel_constant() {
        // 内核常量在 maild::mime，这里不能 link；单测里可以（dev-dependency）。
        assert_eq!(TRUNCATION_MARKER, maild::mime::HTML_TRUNCATION_MARKER);
    }
}
