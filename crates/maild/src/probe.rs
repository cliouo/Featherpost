//! probe：IMAP / SMTP 分阶段探测（ADR-0015 §8）。
//!
//! - IMAP：DNS → TCP → TLS → 认证 → SELECT mailbox → 能力检查 → LOGOUT；
//! - SMTP：DNS → TCP → TLS →（隐式 TLS 读 `220` greeting）→ EHLO →（可选 AUTH PLAIN /
//!   AUTH LOGIN）→ QUIT，**绝不发出 MAIL/RCPT/DATA**；
//! - 结果只返回稳定 stage 与固定的可行动中文诊断（ADR-0042），不回显服务器原文、用户名或秘密；
//! - 每项探测有端到端 deadline，单地址连接与 greeting 读取另有短超时；探测失败是业务结果，
//!   不影响既有连接状态。

use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncWriteExt as _, BufReader};
use tokio::net::TcpStream;

use crate::account_catalog::SecurityMode;
use crate::receive::{read_line_exact, Conn};
use crate::transport::{
    build_tls_connector, expect_class, read_reply, read_reply_exact, select_auth_mechanism,
    smtp_authenticate, write_command, SmtpAuthErrorKind,
};

/// 单个解析地址的 TCP 连接上限（双栈 IPv6 黑洞不能吃掉整个探测 deadline）。
const CONNECT_TIMEOUT_PER_ADDRESS: Duration = Duration::from_secs(5);
/// 等待 greeting 的上限：端口与加密方式配反时（如 465 配 STARTTLS）双方会互等，
/// 必须短于探测 deadline 才能给出可行动诊断而不是笼统的“探测超时”。
const GREETING_TIMEOUT: Duration = Duration::from_secs(8);

/// 认证被拒的固定诊断（IMAP LOGIN 失败 / SMTP 535 等 5xx）。
pub const AUTH_REJECTED_DETAIL: &str = "认证被拒绝（若使用 Gmail/iCloud/QQ/网易邮箱，请确认已开启 IMAP/SMTP 并填入应用专用密码或客户端授权码，而非网页登录密码）";
/// 端口与加密方式匹配的通用提示。
const PORT_SECURITY_HINT: &str =
    "请检查端口与加密方式是否匹配：993/465 通常使用 TLS，143/587 通常使用 STARTTLS";
/// 本模块生成的固定诊断上限（含中文提示，比外部来源 detail 宽）。
const MAX_FIXED_DETAIL_BYTES: usize = 480;

/// 探测推进到的稳定阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeStage {
    Dns,
    Connect,
    Tls,
    Auth,
    Mailbox,
    Capability,
    Ready,
    /// M4-05（ADR-0021）：委托方向经 provider 插件端到端探测。
    Provider,
}

impl ProbeStage {
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeStage::Dns => "dns",
            ProbeStage::Connect => "connect",
            ProbeStage::Tls => "tls",
            ProbeStage::Auth => "auth",
            ProbeStage::Mailbox => "mailbox",
            ProbeStage::Capability => "capability",
            ProbeStage::Ready => "ready",
            ProbeStage::Provider => "provider",
        }
    }
}

/// 单侧探测结果。`ok=false` 时 `stage` 是失败阶段。
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProbeOutcome {
    pub stage: ProbeStage,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl ProbeOutcome {
    fn ok(stage: ProbeStage) -> Self {
        Self {
            stage,
            ok: true,
            detail: String::new(),
        }
    }

    /// 本模块生成的固定文案。
    fn fail(stage: ProbeStage, detail: impl Into<String>) -> Self {
        Self {
            stage,
            ok: false,
            detail: sanitize_with_limit(&detail.into(), MAX_FIXED_DETAIL_BYTES),
        }
    }

    /// 委托方向的 provider 探测结果（M4-05，ADR-0021）：成功无 detail，
    /// 失败 detail 来自插件（有界脱敏）。
    pub fn provider_outcome(ok: bool, detail: impl Into<String>) -> Self {
        if ok {
            Self {
                stage: ProbeStage::Provider,
                ok: true,
                detail: String::new(),
            }
        } else {
            Self {
                stage: ProbeStage::Provider,
                ok: false,
                detail: sanitize_detail(&detail.into()),
            }
        }
    }
}

/// detail 只保留我们生成的固定文案：去控制字符并限长，防服务器原文进入 API 响应。
pub(crate) fn sanitize_detail(detail: &str) -> String {
    const MAX_DETAIL_BYTES: usize = 160;
    sanitize_with_limit(detail, MAX_DETAIL_BYTES)
}

fn sanitize_with_limit(detail: &str, limit: usize) -> String {
    let cleaned: String = detail.chars().filter(|c| !c.is_control()).collect();
    if cleaned.len() <= limit {
        return cleaned;
    }
    let mut end = limit;
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].to_string()
}

/// 收信探测参数（与受管 profile 的 incoming 字段一致）。
#[derive(Clone, Debug)]
pub struct IncomingProbe<'a> {
    pub server: &'a str,
    pub port: u16,
    pub security: SecurityMode,
    pub username: &'a str,
    pub secret: &'a [u8],
    pub mailbox: &'a str,
    /// true 时要求服务器声明 IDLE 能力。
    pub idle: bool,
}

/// 发信探测参数（与受管 profile 的 outgoing 字段一致）。
#[derive(Clone, Debug)]
pub struct OutgoingProbe<'a> {
    pub server: &'a str,
    pub port: u16,
    pub security: SecurityMode,
    pub username: &'a str,
    /// None 表示无认证；复用收件密码时由调用方填入同一秘密。
    pub secret: Option<&'a [u8]>,
}

/// IMAP 全链路探测。总 deadline 内未完成按当前阶段返回超时失败。
pub async fn probe_incoming(probe: IncomingProbe<'_>, deadline: Duration) -> ProbeOutcome {
    let tracker = StageTracker::default();
    let (port, security) = (probe.port, probe.security);
    match tokio::time::timeout(deadline, run_incoming(probe, &tracker)).await {
        Ok(outcome) => outcome,
        Err(_) => timeout_outcome(tracker.current(), port, security),
    }
}

/// SMTP 探测：TLS 后 EHLO、可选 AUTH、QUIT；任何阶段都不投递邮件。
pub async fn probe_outgoing(probe: OutgoingProbe<'_>, deadline: Duration) -> ProbeOutcome {
    let tracker = StageTracker::default();
    let (port, security) = (probe.port, probe.security);
    match tokio::time::timeout(deadline, run_outgoing(probe, &tracker)).await {
        Ok(outcome) => outcome,
        Err(_) => timeout_outcome(tracker.current(), port, security),
    }
}

fn timeout_outcome(stage: ProbeStage, port: u16, security: SecurityMode) -> ProbeOutcome {
    match stage {
        ProbeStage::Tls => ProbeOutcome::fail(stage, with_hint("探测超时", port, security)),
        ProbeStage::Connect => ProbeOutcome::fail(
            stage,
            "探测超时（服务器无响应，请检查服务器地址、端口与网络/防火墙）",
        ),
        _ => ProbeOutcome::fail(stage, "探测超时"),
    }
}

/// 端口与加密方式的可行动提示：常见端口配反时点名，否则给通用匹配规则。
fn port_security_hint(port: u16, security: SecurityMode) -> String {
    match (port, security) {
        (465 | 993, SecurityMode::StartTls) => {
            format!("端口 {port} 通常使用 TLS 直连，当前配置为 STARTTLS，请改为 TLS")
        }
        (25 | 143 | 587, SecurityMode::Implicit) => {
            format!("端口 {port} 通常使用 STARTTLS，当前配置为 TLS 直连，请改为 STARTTLS")
        }
        _ => PORT_SECURITY_HINT.to_string(),
    }
}

fn with_hint(base: &str, port: u16, security: SecurityMode) -> String {
    format!("{base}（{}）", port_security_hint(port, security))
}

struct StageTracker(std::sync::Mutex<ProbeStage>);

impl Default for StageTracker {
    fn default() -> Self {
        Self(std::sync::Mutex::new(ProbeStage::Dns))
    }
}

impl StageTracker {
    fn set(&self, stage: ProbeStage) {
        *self.0.lock().unwrap() = stage;
    }

    fn current(&self) -> ProbeStage {
        *self.0.lock().unwrap()
    }
}

async fn resolve_and_connect(
    server: &str,
    port: u16,
    tracker: &StageTracker,
) -> Result<TcpStream, ProbeOutcome> {
    tracker.set(ProbeStage::Dns);
    let addr = format!("{server}:{port}");
    let resolved = tokio::net::lookup_host(&addr)
        .await
        .map_err(|_| ProbeOutcome::fail(ProbeStage::Dns, "DNS 解析失败"))?;
    let mut last_error = String::from("DNS 无可用地址");
    for socket_address in resolved {
        tracker.set(ProbeStage::Connect);
        match tokio::time::timeout(
            CONNECT_TIMEOUT_PER_ADDRESS,
            TcpStream::connect(socket_address),
        )
        .await
        {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last_error = format!("连接失败: {}", error.kind()),
            Err(_) => last_error = "连接失败: 超时".to_string(),
        }
    }
    Err(ProbeOutcome::fail(
        ProbeStage::Connect,
        format!("{last_error}（请检查服务器地址与端口，以及网络/防火墙是否放行）"),
    ))
}

async fn run_incoming(probe: IncomingProbe<'_>, tracker: &StageTracker) -> ProbeOutcome {
    let stream = match resolve_and_connect(probe.server, probe.port, tracker).await {
        Ok(stream) => stream,
        Err(outcome) => return outcome,
    };

    tracker.set(ProbeStage::Tls);
    let tls = match establish_tls(
        stream,
        probe.server,
        probe.port,
        probe.security,
        ImapStartTls,
        tracker,
    )
    .await
    {
        Ok(tls) => tls,
        Err(outcome) => return outcome,
    };

    tracker.set(ProbeStage::Auth);
    let client = async_imap::Client::new(Box::new(tls) as Conn);
    let password = match std::str::from_utf8(probe.secret) {
        Ok(password) => password.to_string(),
        Err(_) => return ProbeOutcome::fail(ProbeStage::Auth, "密码不是有效的 UTF-8 文本"),
    };
    let mut session = match client.login(probe.username, &password).await {
        Ok(session) => session,
        Err((_error, _client)) => {
            // 不回显服务器认证错误原文。
            return ProbeOutcome::fail(ProbeStage::Auth, AUTH_REJECTED_DETAIL);
        }
    };
    drop(password);

    tracker.set(ProbeStage::Mailbox);
    if session.select(probe.mailbox).await.is_err() {
        return ProbeOutcome::fail(ProbeStage::Mailbox, "邮箱不存在或不可选");
    }

    tracker.set(ProbeStage::Capability);
    if probe.idle {
        match session.capabilities().await {
            Ok(capabilities) if capabilities.has_str("IDLE") => {}
            Ok(_) => return ProbeOutcome::fail(ProbeStage::Capability, "服务器缺少 IDLE 能力"),
            Err(_) => return ProbeOutcome::fail(ProbeStage::Capability, "能力查询失败"),
        }
    }

    let _ = session.logout().await;
    ProbeOutcome::ok(ProbeStage::Ready)
}

async fn run_outgoing(probe: OutgoingProbe<'_>, tracker: &StageTracker) -> ProbeOutcome {
    let stream = match resolve_and_connect(probe.server, probe.port, tracker).await {
        Ok(stream) => stream,
        Err(outcome) => return outcome,
    };

    tracker.set(ProbeStage::Tls);
    let tls = match establish_tls(
        stream,
        probe.server,
        probe.port,
        probe.security,
        SmtpStartTls,
        tracker,
    )
    .await
    {
        Ok(tls) => tls,
        Err(outcome) => return outcome,
    };

    let (read, write) = tokio::io::split(tls);
    let mut reader = BufReader::new(read);
    let mut writer = write;

    // RFC 8314 §3.3：隐式 TLS 握手完成后服务器先发 `220` greeting，读完才能 EHLO。
    // STARTTLS 路径的 greeting 已在明文阶段读过（RFC 3207：升级后不再有 greeting）。
    if probe.security == SecurityMode::Implicit {
        let greeting = match tokio::time::timeout(
            GREETING_TIMEOUT,
            read_reply(&mut reader, "greeting"),
        )
        .await
        {
            Ok(Ok(greeting)) => greeting,
            Ok(Err(_)) | Err(_) => {
                return ProbeOutcome::fail(
                    ProbeStage::Tls,
                    with_hint(
                        "TLS 已建立但未收到 SMTP 220 欢迎行",
                        probe.port,
                        probe.security,
                    ),
                )
            }
        };
        if expect_class(&greeting, "greeting", 2).is_err() {
            return ProbeOutcome::fail(
                ProbeStage::Connect,
                "SMTP 服务器拒绝了连接（欢迎行非 220），请稍后重试或检查服务器地址",
            );
        }
    }

    // RFC 3207：STARTTLS 之后必须重新 EHLO；implicit 在 greeting 之后首次 EHLO。
    tracker.set(ProbeStage::Capability);
    if write_command(&mut writer, b"EHLO maild\r\n", "EHLO")
        .await
        .is_err()
    {
        return ProbeOutcome::fail(ProbeStage::Capability, "写入 EHLO 失败");
    }
    let ehlo = match read_reply(&mut reader, "EHLO").await {
        Ok(ehlo) => ehlo,
        Err(_) => return ProbeOutcome::fail(ProbeStage::Capability, "读取 EHLO 失败"),
    };
    if expect_class(&ehlo, "EHLO", 2).is_err() {
        return ProbeOutcome::fail(ProbeStage::Capability, "EHLO 被拒");
    }

    if !probe.username.is_empty() {
        let mechanism = match select_auth_mechanism(&ehlo) {
            Ok(mechanism) => mechanism,
            Err(None) => {
                return ProbeOutcome::fail(
                    ProbeStage::Capability,
                    "服务器未声明 AUTH 能力（请确认使用的是邮件提交端口 465 或 587，而非 25）",
                )
            }
            Err(Some(_)) => {
                return ProbeOutcome::fail(
                    ProbeStage::Capability,
                    "服务器未提供 PLAIN/LOGIN 认证方式（可能仅支持 OAuth2 登录，暂不支持）",
                )
            }
        };
        tracker.set(ProbeStage::Auth);
        let Some(secret) = probe.secret else {
            return ProbeOutcome::fail(ProbeStage::Auth, "缺少发信凭据");
        };
        let password = match std::str::from_utf8(secret) {
            Ok(password) => password,
            Err(_) => return ProbeOutcome::fail(ProbeStage::Auth, "密码不是有效的 UTF-8 文本"),
        };
        if let Err(error) =
            smtp_authenticate(&mut reader, &mut writer, mechanism, probe.username, password).await
        {
            // 只按分类给固定文案，绝不回显服务器原文或秘密。
            let detail = match error.kind {
                SmtpAuthErrorKind::Io => "认证过程中连接中断或超时",
                SmtpAuthErrorKind::Temporary => "认证暂时失败（服务器返回临时错误），请稍后重试",
                SmtpAuthErrorKind::Rejected => AUTH_REJECTED_DETAIL,
            };
            return ProbeOutcome::fail(ProbeStage::Auth, detail);
        }
    }

    let _ = write_command(&mut writer, b"QUIT\r\n", "QUIT").await;
    ProbeOutcome::ok(ProbeStage::Ready)
}

// —— TLS 建立：IMAP 与 SMTP 各自的 STARTTLS 明文协商 ——

async fn establish_tls<S>(
    stream: TcpStream,
    server: &str,
    port: u16,
    security: SecurityMode,
    negotiation: S,
    _tracker: &StageTracker,
) -> Result<tokio_native_tls::TlsStream<TcpStream>, ProbeOutcome>
where
    S: StartTlsNegotiation,
{
    let connector =
        build_tls_connector(false).map_err(|_| ProbeOutcome::fail(ProbeStage::Tls, "TLS 初始化失败"))?;
    let stream = match security {
        SecurityMode::Implicit => stream,
        SecurityMode::StartTls => negotiation.negotiate(stream).await.map_err(|outcome| {
            // 明文协商阶段的 TLS 类失败最常见的根因是端口配反（如 465/993 配 STARTTLS）。
            if outcome.stage == ProbeStage::Tls {
                ProbeOutcome::fail(ProbeStage::Tls, with_hint(&outcome.detail, port, security))
            } else {
                outcome
            }
        })?,
    };
    // 隐式 TLS 打到明文端口时，服务器会等一整行命令、客户端等 TLS 记录，双方互等：
    // 握手同样受 greeting 级短超时约束，给出端口/加密方式提示。
    match tokio::time::timeout(GREETING_TIMEOUT, connector.connect(server, stream)).await {
        Ok(Ok(tls)) => Ok(tls),
        Ok(Err(_)) => Err(ProbeOutcome::fail(
            ProbeStage::Tls,
            format!(
                "TLS 握手失败（{}；或服务器证书不受信任）",
                port_security_hint(port, security)
            ),
        )),
        Err(_) => Err(ProbeOutcome::fail(
            ProbeStage::Tls,
            with_hint("TLS 握手超时", port, security),
        )),
    }
}

trait StartTlsNegotiation: Send + Sync {
    fn negotiate(
        &self,
        stream: TcpStream,
    ) -> impl std::future::Future<Output = Result<TcpStream, ProbeOutcome>> + Send;
}

/// IMAP STARTTLS：greeting → `a0 STARTTLS` → OK（逐字节读，不预取握手字节）。
struct ImapStartTls;

impl StartTlsNegotiation for ImapStartTls {
    async fn negotiate(&self, mut stream: TcpStream) -> Result<TcpStream, ProbeOutcome> {
        let fail =
            |detail: &str| ProbeOutcome::fail(ProbeStage::Tls, detail);
        let greeting = tokio::time::timeout(GREETING_TIMEOUT, read_line_exact(&mut stream))
            .await
            .map_err(|_| fail("等待 IMAP greeting 超时"))?
            .map_err(|_| fail("读取 greeting 失败"))?;
        if !greeting.starts_with("* OK") && !greeting.starts_with("* PREAUTH") {
            return Err(fail("IMAP greeting 异常"));
        }
        stream
            .write_all(b"a0 STARTTLS\r\n")
            .await
            .map_err(|_| fail("写入 STARTTLS 失败"))?;
        let response = read_line_exact(&mut stream)
            .await
            .map_err(|_| fail("读取 STARTTLS 响应失败"))?;
        if !response.starts_with("a0 OK") {
            return Err(fail("服务器不支持 STARTTLS"));
        }
        Ok(stream)
    }
}

/// SMTP STARTTLS：greeting → EHLO → 校验能力 → STARTTLS 220。
struct SmtpStartTls;

impl StartTlsNegotiation for SmtpStartTls {
    async fn negotiate(&self, mut stream: TcpStream) -> Result<TcpStream, ProbeOutcome> {
        let fail = |detail: &str| ProbeOutcome::fail(ProbeStage::Tls, detail);
        let capability_fail =
            |detail: &str| ProbeOutcome::fail(ProbeStage::Capability, detail);
        let greeting =
            tokio::time::timeout(GREETING_TIMEOUT, read_reply_exact(&mut stream, "greeting"))
                .await
                .map_err(|_| fail("等待 SMTP greeting 超时"))?
                .map_err(|_| fail("读取 greeting 失败"))?;
        expect_class(&greeting, "greeting", 2).map_err(|_| fail("SMTP greeting 被拒"))?;

        write_command(&mut stream, b"EHLO maild\r\n", "首次 EHLO")
            .await
            .map_err(|_| capability_fail("写入 EHLO 失败"))?;
        let ehlo = read_reply_exact(&mut stream, "首次 EHLO")
            .await
            .map_err(|_| capability_fail("读取 EHLO 失败"))?;
        expect_class(&ehlo, "首次 EHLO", 2).map_err(|_| capability_fail("EHLO 被拒"))?;
        if !ehlo.has_capability("STARTTLS") {
            return Err(capability_fail("服务器缺少 STARTTLS 能力"));
        }
        write_command(&mut stream, b"STARTTLS\r\n", "STARTTLS")
            .await
            .map_err(|_| fail("写入 STARTTLS 失败"))?;
        let starttls = read_reply_exact(&mut stream, "STARTTLS")
            .await
            .map_err(|_| fail("读取 STARTTLS 响应失败"))?;
        if starttls.code != 220 {
            return Err(fail("STARTTLS 被拒"));
        }
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_security_hint_names_common_mismatches() {
        let hint = port_security_hint(465, SecurityMode::StartTls);
        assert!(hint.contains("465") && hint.contains("请改为 TLS"), "{hint}");
        let hint = port_security_hint(993, SecurityMode::StartTls);
        assert!(hint.contains("993") && hint.contains("请改为 TLS"), "{hint}");
        let hint = port_security_hint(587, SecurityMode::Implicit);
        assert!(hint.contains("587") && hint.contains("请改为 STARTTLS"), "{hint}");
        let hint = port_security_hint(143, SecurityMode::Implicit);
        assert!(hint.contains("143") && hint.contains("请改为 STARTTLS"), "{hint}");
        assert_eq!(port_security_hint(465, SecurityMode::Implicit), PORT_SECURITY_HINT);
        assert_eq!(port_security_hint(2525, SecurityMode::StartTls), PORT_SECURITY_HINT);
    }

    #[test]
    fn fixed_details_are_not_truncated_but_external_details_are() {
        let outcome = ProbeOutcome::fail(ProbeStage::Auth, AUTH_REJECTED_DETAIL);
        assert_eq!(outcome.detail, AUTH_REJECTED_DETAIL);
        let tls = ProbeOutcome::fail(
            ProbeStage::Tls,
            with_hint("TLS 已建立但未收到 SMTP 220 欢迎行", 587, SecurityMode::Implicit),
        );
        assert!(tls.detail.ends_with("）"), "{}", tls.detail);

        let long = "x".repeat(400);
        let provider = ProbeOutcome::provider_outcome(false, long);
        assert_eq!(provider.detail.len(), 160);
        let control = ProbeOutcome::provider_outcome(false, "a\r\nb");
        assert_eq!(control.detail, "ab");
    }
}
