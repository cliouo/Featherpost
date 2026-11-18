//! transport：发送（SMTP，spine 服务之一）。
//!
//! 服务：`Transport`——最小 SMTP 客户端（EHLO / 可选 STARTTLS 或隐式 TLS / MAIL FROM /
//! RCPT TO / DATA，可选 AUTH PLAIN 或 AUTH LOGIN）。发送走 §6 transport seam 的 smtp
//! provider；resend/mailgun 类 Web API provider 是后续插件。
//!
//! ADR-0042：隐式 TLS（465）握手后先读 `220` greeting 再 EHLO（RFC 8314 §3.3）；
//! AUTH 按 EHLO 宣告的机制在 PLAIN / LOGIN 间协商；多地址逐个连接（单地址 10s），
//! 会话内每步读写 30s 超时，防双栈黑洞与服务器假死挂住发信。

use std::any::TypeId;
use std::sync::Arc;
use std::time::Duration;

use cordis::{Context, Plugin, PluginError, Service};
use serde::Deserialize;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

const MAX_MAILBOX_BYTES: usize = 320;
const MAX_SUBJECT_BYTES: usize = 998;
const MAX_BODY_BYTES: usize = 512 * 1024;

/// DNS 解析上限。
const SMTP_RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
/// 单个解析地址的 TCP 连接上限：双栈网络里 IPv6 黑洞不能吃掉整次发信。
const SMTP_CONNECT_TIMEOUT_PER_ADDRESS: Duration = Duration::from_secs(10);
/// 会话内每条命令写入 / 每个响应读取的上限。
pub(crate) const SMTP_STEP_TIMEOUT: Duration = Duration::from_secs(30);
/// DATA 结束符之后等待最终响应的上限（服务器常在此阶段做内容扫描，RFC 5321 建议 10 分钟）。
const SMTP_DATA_ACCEPT_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug, Deserialize)]
pub struct TransportConfig {
    #[serde(default)]
    pub server: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// 空 = 无认证；非空按 EHLO 宣告协商 AUTH PLAIN / AUTH LOGIN（ADR-0042）。
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    /// "none"（默认，明文）| "starttls"（EHLO 后原连接升级）| "implicit"（465 风格隐式 TLS）。
    #[serde(default = "default_tls")]
    pub tls: String,
    /// 跳过证书校验（仅测试/自签场景；生产必须 false，走系统根证书）。
    #[serde(default)]
    pub accept_invalid_certs: bool,
}

fn default_port() -> u16 {
    25
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

fn default_tls() -> String {
    "none".into()
}

impl Default for TransportConfig {
    /// 与 serde 默认值一致。
    fn default() -> Self {
        TransportConfig {
            server: String::new(),
            port: default_port(),
            username: String::new(),
            password: String::new(),
            tls: default_tls(),
            accept_invalid_certs: false,
        }
    }
}

impl Service for TransportConfig {
    fn name(&self) -> &'static str {
        "transport.config"
    }
}

/// 发送服务（spine `ctx.transport` 的最小形态）。
pub struct Transport {
    cfg: TransportConfig,
}

impl Service for Transport {
    fn name(&self) -> &'static str {
        "transport"
    }
}

#[derive(Debug)]
pub struct DeliveryFailure {
    pub uncertain: bool,
    pub detail: String,
}

impl Transport {
    /// 用任意参数构造发送器（per-account 发信复用同一实现）。
    pub fn from_config(cfg: TransportConfig) -> Self {
        Self { cfg }
    }

    /// 发一封纯文本邮件。错误包含失败阶段和响应码，不回显可能携带凭据的服务端文本。
    ///
    /// 保留给既有调用方与 `send` RPC 的最简形态；富消息走 [`Transport::send_message`]。
    pub async fn send(
        &self,
        from: &str,
        to: &str,
        subject: &str,
        body: &str,
    ) -> Result<(), String> {
        validate_message(from, to, subject, body)?;
        let message = crate::compose::OutgoingMessage {
            from: from.to_string(),
            to: vec![to.to_string()],
            subject: subject.to_string(),
            body: body.to_string(),
            message_id: crate::compose::new_message_id(from, unix_now()),
            date_unix: unix_now(),
            ..Default::default()
        };
        self.send_message(&message).await.map(|_| ())
    }

    /// 组装并投递一封完整的邮件（ADR-0028）。返回组装出的报文——调用方需要它来
    /// `APPEND` 到已发送，且必须是**同一份**字节。
    ///
    /// 信封收件人是 to + cc + bcc 的并集；**Bcc 只在这里出现，不在报文头里**。
    pub async fn send_message(
        &self,
        message: &crate::compose::OutgoingMessage,
    ) -> Result<String, String> {
        self.send_message_checked(message)
            .await
            .map_err(|e| e.detail)
    }
    pub async fn send_message_checked(
        &self,
        message: &crate::compose::OutgoingMessage,
    ) -> Result<String, DeliveryFailure> {
        let recipients = message.envelope_recipients();
        if recipients.is_empty() || recipients.len() > crate::compose::MAX_RECIPIENTS {
            return Err(DeliveryFailure {
                uncertain: false,
                detail: "收件人数量无效".into(),
            });
        }
        let raw = crate::compose::assemble(message);
        let mut uncertain = false;
        self.deliver(&message.from, &recipients, &raw, &mut uncertain)
            .await
            .map_err(|detail| DeliveryFailure { uncertain, detail })?;
        Ok(raw)
    }

    async fn deliver(
        &self,
        from: &str,
        recipients: &[String],
        raw: &str,
        uncertain: &mut bool,
    ) -> Result<(), String> {
        if !matches!(self.cfg.tls.as_str(), "none" | "starttls" | "implicit") {
            return Err(format!(
                "SMTP TLS 模式无效: {}（仅支持 none/starttls/implicit）",
                self.cfg.tls
            ));
        }

        let stream = connect_smtp(&self.cfg.server, self.cfg.port).await?;

        match self.cfg.tls.as_str() {
            "none" => {
                self.run_transaction(stream, true, from, recipients, raw, uncertain)
                    .await
            }
            "starttls" => {
                let tls = self.upgrade_starttls(stream).await?;
                self.run_transaction(tls, false, from, recipients, raw, uncertain)
                    .await
            }
            "implicit" => {
                // 465 风格（RFC 8314 §3.3）：connect 后立即 TLS 握手；握手完成后服务器
                // **先发 `220` greeting**，读完才能 EHLO——否则会把 220 当成 EHLO 响应，
                // 后续 AUTH/MAIL 全部错位。
                let connector = build_tls_connector(self.cfg.accept_invalid_certs)?;
                let tls = with_step_timeout(
                    connector.connect(&self.cfg.server, stream),
                    "SMTP TLS 握手超时",
                )
                .await?
                .map_err(|e| format!("SMTP TLS 握手失败: {e}"))?;
                self.run_transaction(tls, true, from, recipients, raw, uncertain)
                    .await
            }
            _ => unreachable!("TLS 模式已校验"),
        }
    }

    async fn upgrade_starttls(
        &self,
        mut stream: TcpStream,
    ) -> Result<tokio_native_tls::TlsStream<TcpStream>, String> {
        // STARTTLS 响应后紧跟 TLS 帧；协商阶段逐字节读，不能让 BufReader 预取握手字节。
        let greeting = read_reply_exact(&mut stream, "greeting").await?;
        expect_class(&greeting, "greeting", 2)?;

        write_command(&mut stream, b"EHLO maild\r\n", "首次 EHLO").await?;
        let ehlo = read_reply_exact(&mut stream, "首次 EHLO").await?;
        expect_class(&ehlo, "首次 EHLO", 2)?;
        if !ehlo.has_capability("STARTTLS") {
            return Err("SMTP 首次 EHLO 未声明 STARTTLS，拒绝降级为明文".into());
        }

        write_command(&mut stream, b"STARTTLS\r\n", "STARTTLS").await?;
        let starttls = read_reply_exact(&mut stream, "STARTTLS").await?;
        if starttls.code != 220 {
            return Err(format!(
                "SMTP STARTTLS 被拒（期望 220，收到 {}）",
                starttls.code
            ));
        }

        let connector = build_tls_connector(self.cfg.accept_invalid_certs)?;
        with_step_timeout(
            connector.connect(&self.cfg.server, stream),
            "SMTP TLS 握手超时",
        )
        .await?
        .map_err(|e| format!("SMTP TLS 握手失败: {e}"))
    }

    async fn run_transaction<S>(
        &self,
        stream: S,
        expect_greeting: bool,
        from: &str,
        recipients: &[String],
        raw: &str,
        uncertain: &mut bool,
    ) -> Result<(), String>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (r, mut w) = tokio::io::split(stream);
        let mut reader = BufReader::new(r);

        if expect_greeting {
            let greeting = read_reply(&mut reader, "greeting").await?;
            expect_class(&greeting, "greeting", 2)?;
        }

        let ehlo_phase = if expect_greeting {
            "EHLO"
        } else {
            "TLS 后 EHLO"
        };
        write_command(&mut w, b"EHLO maild\r\n", ehlo_phase).await?;
        let ehlo = read_reply(&mut reader, ehlo_phase).await?;
        expect_class(&ehlo, ehlo_phase, 2)?;

        if !self.cfg.username.is_empty() {
            // 未宣告 AUTH 时保留既有行为（直接尝试 PLAIN，由服务器裁决）。
            let mechanism = select_auth_mechanism(&ehlo).unwrap_or(AuthMechanism::Plain);
            smtp_authenticate(
                &mut reader,
                &mut w,
                mechanism,
                &self.cfg.username,
                &self.cfg.password,
            )
            .await
            .map_err(|error| error.detail)?;
        }

        write_command(
            &mut w,
            format!("MAIL FROM:<{from}>\r\n").as_bytes(),
            "MAIL FROM",
        )
        .await?;
        let mail_from = read_reply(&mut reader, "MAIL FROM").await?;
        expect_class(&mail_from, "MAIL FROM", 2)?;
        for recipient in recipients {
            write_command(
                &mut w,
                format!("RCPT TO:<{recipient}>\r\n").as_bytes(),
                "RCPT TO",
            )
            .await?;
            let rcpt_to = read_reply(&mut reader, "RCPT TO").await?;
            expect_class(&rcpt_to, "RCPT TO", 2)?;
        }
        write_command(&mut w, b"DATA\r\n", "DATA").await?;
        let data = read_reply(&mut reader, "DATA").await?;
        expect_class(&data, "DATA", 3)?;

        let payload = format!("{}.\r\n", encode_smtp_body(raw));
        *uncertain = true;
        write_command_within(
            &mut w,
            payload.as_bytes(),
            "DATA 内容",
            data_write_timeout(payload.len()),
        )
        .await?;
        let data_body =
            read_reply_within(&mut reader, "DATA 内容", SMTP_DATA_ACCEPT_TIMEOUT).await?;
        if matches!(data_body.code / 100, 4 | 5) {
            *uncertain = false;
        }
        expect_class(&data_body, "DATA 内容", 2)?;

        // DATA 2xx is the acceptance boundary. QUIT cannot turn an accepted message into failure.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            write_command(&mut w, b"QUIT\r\n", "QUIT"),
        )
        .await;
        Ok(())
    }
}

pub(crate) struct SmtpReply {
    pub(crate) code: u16,
    lines: Vec<String>,
}

impl SmtpReply {
    pub(crate) fn has_capability(&self, capability: &str) -> bool {
        self.lines.iter().any(|line| {
            line.get(4..)
                .and_then(|text| text.split_ascii_whitespace().next())
                .is_some_and(|word| word.eq_ignore_ascii_case(capability))
        })
    }

    /// EHLO 是否宣告了认证能力（`AUTH ...` 或旧式 `AUTH=...`）。
    pub(crate) fn advertises_auth(&self) -> bool {
        self.has_capability("AUTH") || self.auth_lines().next().is_some()
    }

    /// EHLO 宣告的 SASL 机制（大写、去重、保序）。同时兼容 `250-AUTH PLAIN LOGIN`
    /// 与旧式 `250-AUTH=PLAIN LOGIN`，关键字大小写不敏感。
    pub(crate) fn auth_mechanisms(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for mechanisms in self.auth_lines() {
            for mechanism in mechanisms {
                let mechanism = mechanism.to_ascii_uppercase();
                if !mechanism.is_empty() && !out.contains(&mechanism) {
                    out.push(mechanism);
                }
            }
        }
        out
    }

    /// 每个 AUTH 能力行里的机制 token 序列。
    fn auth_lines(&self) -> impl Iterator<Item = Vec<&str>> {
        self.lines.iter().filter_map(|line| {
            let mut words = line.get(4..)?.split_ascii_whitespace();
            let first = words.next()?;
            let keyword = first.get(..4)?;
            if !keyword.eq_ignore_ascii_case("AUTH") {
                return None;
            }
            let rest = &first[4..];
            let mut mechanisms = Vec::new();
            if let Some(inline) = rest.strip_prefix('=') {
                mechanisms.push(inline);
            } else if !rest.is_empty() {
                return None; // 例如 AUTHX：不是 AUTH 关键字
            }
            mechanisms.extend(words);
            Some(mechanisms)
        })
    }
}

/// 本客户端支持的 SASL 机制。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthMechanism {
    Plain,
    Login,
}

/// 按 EHLO 宣告选择认证机制：
/// - 宣告了 PLAIN，或只有裸 `AUTH` 未列子机制 → PLAIN；
/// - 未宣告 PLAIN 但宣告了 LOGIN → LOGIN；
/// - 完全未宣告 AUTH → `Err(None)`（由调用方决定是否硬试）；
/// - 只宣告了其他机制（如 XOAUTH2 / CRAM-MD5）→ `Err(Some(机制列表))`。
pub(crate) fn select_auth_mechanism(ehlo: &SmtpReply) -> Result<AuthMechanism, Option<Vec<String>>> {
    if !ehlo.advertises_auth() {
        return Err(None);
    }
    let mechanisms = ehlo.auth_mechanisms();
    if mechanisms.is_empty() || mechanisms.iter().any(|m| m == "PLAIN") {
        Ok(AuthMechanism::Plain)
    } else if mechanisms.iter().any(|m| m == "LOGIN") {
        Ok(AuthMechanism::Login)
    } else {
        Err(Some(mechanisms))
    }
}

/// AUTH 失败的分类：probe 据此给固定中文诊断，transport 直接用 `detail`。
#[derive(Debug)]
pub(crate) struct SmtpAuthError {
    pub(crate) kind: SmtpAuthErrorKind,
    /// 不含服务器原文与秘密（只有阶段与响应码）。
    pub(crate) detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SmtpAuthErrorKind {
    /// 网络读写失败或超时。
    Io,
    /// 服务器给了 4xx（临时失败）。
    Temporary,
    /// 服务器给了 5xx 或非预期响应码（凭据被拒）。
    Rejected,
}

/// 执行一次 SMTP 认证。所有含秘密的临时缓冲（明文拼接、base64、命令行）用
/// [`Zeroizing`] 包裹，写出后立即清零释放。
pub(crate) async fn smtp_authenticate<R, W>(
    reader: &mut R,
    writer: &mut W,
    mechanism: AuthMechanism,
    username: &str,
    password: &str,
) -> Result<(), SmtpAuthError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    match mechanism {
        AuthMechanism::Plain => {
            let command = {
                let plain = Zeroizing::new(format!("\0{username}\0{password}"));
                let encoded = Zeroizing::new(b64_encode(plain.as_bytes()));
                Zeroizing::new(format!("AUTH PLAIN {}\r\n", encoded.as_str()))
            };
            auth_write(writer, command.as_bytes(), "AUTH").await?;
            drop(command);
            let reply = auth_read(reader, "AUTH").await?;
            auth_expect(&reply, "AUTH", 2)
        }
        AuthMechanism::Login => {
            auth_write(writer, b"AUTH LOGIN\r\n", "AUTH LOGIN").await?;
            let challenge = auth_read(reader, "AUTH LOGIN").await?;
            auth_expect_code(&challenge, "AUTH LOGIN", 334)?;

            let user_line = {
                let encoded = Zeroizing::new(b64_encode(username.as_bytes()));
                Zeroizing::new(format!("{}\r\n", encoded.as_str()))
            };
            auth_write(writer, user_line.as_bytes(), "AUTH LOGIN 用户名").await?;
            drop(user_line);
            let challenge = auth_read(reader, "AUTH LOGIN 用户名").await?;
            auth_expect_code(&challenge, "AUTH LOGIN 用户名", 334)?;

            let pass_line = {
                let encoded = Zeroizing::new(b64_encode(password.as_bytes()));
                Zeroizing::new(format!("{}\r\n", encoded.as_str()))
            };
            auth_write(writer, pass_line.as_bytes(), "AUTH LOGIN 密码").await?;
            drop(pass_line);
            let reply = auth_read(reader, "AUTH LOGIN 密码").await?;
            auth_expect(&reply, "AUTH", 2)
        }
    }
}

async fn auth_write<W>(writer: &mut W, bytes: &[u8], phase: &str) -> Result<(), SmtpAuthError>
where
    W: AsyncWrite + Unpin,
{
    write_command(writer, bytes, phase)
        .await
        .map_err(|detail| SmtpAuthError {
            kind: SmtpAuthErrorKind::Io,
            detail,
        })
}

async fn auth_read<R>(reader: &mut R, phase: &str) -> Result<SmtpReply, SmtpAuthError>
where
    R: AsyncBufRead + Unpin,
{
    read_reply(reader, phase)
        .await
        .map_err(|detail| SmtpAuthError {
            kind: SmtpAuthErrorKind::Io,
            detail,
        })
}

fn auth_rejected(reply: &SmtpReply, phase: &str) -> SmtpAuthError {
    SmtpAuthError {
        kind: if reply.code / 100 == 4 {
            SmtpAuthErrorKind::Temporary
        } else {
            SmtpAuthErrorKind::Rejected
        },
        detail: format!("SMTP {phase} 被拒（收到 {}）", reply.code),
    }
}

fn auth_expect(reply: &SmtpReply, phase: &str, class: u16) -> Result<(), SmtpAuthError> {
    if reply.code / 100 == class {
        Ok(())
    } else {
        Err(auth_rejected(reply, phase))
    }
}

fn auth_expect_code(reply: &SmtpReply, phase: &str, code: u16) -> Result<(), SmtpAuthError> {
    if reply.code == code {
        Ok(())
    } else {
        Err(auth_rejected(reply, phase))
    }
}

/// 多地址解析后逐个连接（单地址 10s）。错误不含服务器原文。
async fn connect_smtp(server: &str, port: u16) -> Result<TcpStream, String> {
    let target = format!("{server}:{port}");
    let addresses: Vec<std::net::SocketAddr> =
        match tokio::time::timeout(SMTP_RESOLVE_TIMEOUT, tokio::net::lookup_host((server, port)))
            .await
        {
            Ok(Ok(addresses)) => addresses.collect(),
            Ok(Err(e)) => return Err(format!("连接 SMTP {target} 失败: DNS 解析失败: {e}")),
            Err(_) => return Err(format!("连接 SMTP {target} 失败: DNS 解析超时")),
        };
    if addresses.is_empty() {
        return Err(format!("连接 SMTP {target} 失败: DNS 无可用地址"));
    }
    let mut last_error = String::new();
    for address in addresses {
        match tokio::time::timeout(SMTP_CONNECT_TIMEOUT_PER_ADDRESS, TcpStream::connect(address))
            .await
        {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last_error = format!("{address}: {e}"),
            Err(_) => last_error = format!("{address}: 连接超时"),
        }
    }
    Err(format!("连接 SMTP {target} 失败: {last_error}"))
}

async fn with_step_timeout<F, T>(future: F, timeout_detail: &str) -> Result<T, String>
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(SMTP_STEP_TIMEOUT, future)
        .await
        .map_err(|_| timeout_detail.to_string())
}

/// DATA 正文写入上限：30s 基线 + 每 16 KiB 1s（慢上行也能送完大附件），封顶 15 分钟。
fn data_write_timeout(len: usize) -> Duration {
    let scaled = SMTP_STEP_TIMEOUT.as_secs() + (len as u64) / (16 * 1024);
    Duration::from_secs(scaled.min(15 * 60))
}

pub(crate) async fn write_command<W>(writer: &mut W, command: &[u8], phase: &str) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    write_command_within(writer, command, phase, SMTP_STEP_TIMEOUT).await
}

async fn write_command_within<W>(
    writer: &mut W,
    command: &[u8],
    phase: &str,
    limit: Duration,
) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    let write = async {
        writer.write_all(command).await?;
        writer.flush().await
    };
    match tokio::time::timeout(limit, write).await {
        Ok(result) => result.map_err(|e| format!("SMTP {phase} 写入失败: {e}")),
        Err(_) => Err(format!("SMTP {phase} 写入超时")),
    }
}

pub(crate) async fn read_reply<R>(reader: &mut R, phase: &str) -> Result<SmtpReply, String>
where
    R: AsyncBufRead + Unpin,
{
    read_reply_within(reader, phase, SMTP_STEP_TIMEOUT).await
}

async fn read_reply_within<R>(reader: &mut R, phase: &str, limit: Duration) -> Result<SmtpReply, String>
where
    R: AsyncBufRead + Unpin,
{
    tokio::time::timeout(limit, read_reply_untimed(reader, phase))
        .await
        .unwrap_or_else(|_| Err(format!("SMTP {phase} 等待响应超时")))
}

async fn read_reply_untimed<R>(reader: &mut R, phase: &str) -> Result<SmtpReply, String>
where
    R: AsyncBufRead + Unpin,
{
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("SMTP {phase} 读取响应失败: {e}"))?;
        if n == 0 {
            return Err(format!("SMTP {phase} 响应前连接被服务端关闭"));
        }
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        let continued = reply_continues(&line, phase)?;
        lines.push(line);
        if !continued {
            return make_reply(lines, phase);
        }
    }
}

pub(crate) async fn read_reply_exact(stream: &mut TcpStream, phase: &str) -> Result<SmtpReply, String> {
    tokio::time::timeout(SMTP_STEP_TIMEOUT, read_reply_exact_untimed(stream, phase))
        .await
        .unwrap_or_else(|_| Err(format!("SMTP {phase} 等待响应超时")))
}

async fn read_reply_exact_untimed(stream: &mut TcpStream, phase: &str) -> Result<SmtpReply, String> {
    let mut lines = Vec::new();
    loop {
        let line = read_line_exact(stream)
            .await
            .map_err(|e| format!("SMTP {phase} 读取响应失败: {e}"))?;
        let continued = reply_continues(&line, phase)?;
        lines.push(line);
        if !continued {
            return make_reply(lines, phase);
        }
    }
}

fn reply_continues(line: &str, phase: &str) -> Result<bool, String> {
    if line.len() < 4 || !line.as_bytes()[..3].iter().all(u8::is_ascii_digit) {
        return Err(format!("SMTP {phase} 响应格式无效"));
    }
    match line.as_bytes()[3] {
        b'-' => Ok(true),
        b' ' => Ok(false),
        _ => Err(format!("SMTP {phase} 响应格式无效")),
    }
}

fn make_reply(lines: Vec<String>, phase: &str) -> Result<SmtpReply, String> {
    let code = lines[0][..3]
        .parse::<u16>()
        .map_err(|e| format!("SMTP {phase} 响应码无效: {e}"))?;
    if lines.iter().any(|line| line[..3] != lines[0][..3]) {
        return Err(format!("SMTP {phase} 多行响应码不一致"));
    }
    Ok(SmtpReply { code, lines })
}

pub(crate) fn expect_class(reply: &SmtpReply, phase: &str, expected: u16) -> Result<(), String> {
    if reply.code / 100 != expected {
        return Err(format!("SMTP {phase} 被拒（收到 {}）", reply.code));
    }
    Ok(())
}

pub(crate) fn build_tls_connector(
    accept_invalid_certs: bool,
) -> Result<tokio_native_tls::TlsConnector, String> {
    let mut builder = native_tls::TlsConnector::builder();
    // 受管路径恒传 false；只有 debug 构建下的 hermetic 环境变量能打开自签豁免。
    if accept_invalid_certs || crate::credential::test_accept_invalid_certs() {
        builder.danger_accept_invalid_certs(true);
    }
    let connector = builder
        .build()
        .map_err(|e| format!("SMTP TLS connector 构建失败: {e}"))?;
    Ok(tokio_native_tls::TlsConnector::from(connector))
}

pub(crate) async fn read_line_exact<S>(stream: &mut S) -> Result<String, tokio::io::Error>
where
    S: AsyncRead + Unpin,
{
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Err(tokio::io::Error::new(
                tokio::io::ErrorKind::UnexpectedEof,
                "连接在 SMTP 响应行结束前关闭",
            ));
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&line)
        .trim_end_matches('\r')
        .to_string())
}

pub(crate) fn validate_message(
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    validate_mailbox("from", from)?;
    validate_mailbox("to", to)?;
    if subject.len() > MAX_SUBJECT_BYTES {
        return Err(format!("subject 过长（上限 {MAX_SUBJECT_BYTES} 字节）"));
    }
    if subject.chars().any(char::is_control) {
        return Err("subject 不能包含控制字符".into());
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(format!("body 过长（上限 {MAX_BODY_BYTES} 字节）"));
    }
    if body.contains('\0') {
        return Err("body 不能包含 NUL".into());
    }
    Ok(())
}

pub(crate) fn validate_mailbox(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} 必填"));
    }
    if value.len() > MAX_MAILBOX_BYTES {
        return Err(format!("{label} 过长（上限 {MAX_MAILBOX_BYTES} 字节）"));
    }
    let Some((local, domain)) = value.split_once('@') else {
        return Err(format!("{label} 不是有效邮箱地址"));
    };
    let local_valid = !local.is_empty()
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'/'
                        | b'='
                        | b'?'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'{'
                        | b'|'
                        | b'}'
                        | b'~'
                        | b'.'
                )
        });
    let domain_valid = !domain.is_empty()
        && domain.len() <= 253
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    if !local_valid || !domain_valid {
        return Err(format!("{label} 不是有效邮箱地址"));
    }
    Ok(())
}

/// SMTP DATA 传输：统一 CRLF，并对行首 `.` 做 dot-stuffing。
fn encode_smtp_body(body: &str) -> String {
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len() + 2);
    let mut lines = normalized.split('\n').peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() && lines.peek().is_none() && normalized.ends_with('\n') {
            break;
        }
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

/// 最小 base64（标准字母表 + 填充），AUTH PLAIN / AUTH LOGIN 与 MIME 编码共用。
pub fn b64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// transport 插件：inject 配置 → provide Transport。
pub struct TransportPlugin;

impl Plugin for TransportPlugin {
    fn inject(&self) -> Vec<TypeId> {
        vec![TypeId::of::<TransportConfig>()]
    }

    fn apply(&self, ctx: &Context) -> Result<(), PluginError> {
        let cfg = ctx.get::<TransportConfig>().expect("inject 就绪");
        ctx.provide::<Transport>(Arc::new(Transport {
            cfg: cfg.as_ref().clone(),
        }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_smtp_command_and_header_injection() {
        assert!(validate_message(
            "a@test.local\r\nRCPT TO:<x@test.local>",
            "b@test.local",
            "",
            ""
        )
        .is_err());
        assert!(validate_message("a@test.local", "b@test.local", "ok\r\nBcc: x", "").is_err());
        assert!(validate_message("a,b@test.local", "b@test.local", "", "").is_err());
        assert!(validate_message("a@test.local", "b@-test.local", "", "").is_err());
        assert!(validate_message("first.last+tag@test.local", "b@test.local", "", "").is_ok());
    }

    fn reply(lines: &[&str]) -> SmtpReply {
        make_reply(lines.iter().map(|line| line.to_string()).collect(), "EHLO").unwrap()
    }

    #[test]
    fn auth_mechanisms_parse_both_syntaxes_case_insensitively() {
        let ehlo = reply(&[
            "250-mx.example",
            "250-auth plain Login",
            "250-AUTH=PLAIN LOGIN XOAUTH2",
            "250-AUTHX FOO",
            "250 SIZE 1000",
        ]);
        assert!(ehlo.advertises_auth());
        assert_eq!(ehlo.auth_mechanisms(), vec!["PLAIN", "LOGIN", "XOAUTH2"]);

        let legacy_only = reply(&["250-mx.example", "250 AUTH=LOGIN"]);
        assert!(legacy_only.advertises_auth());
        assert_eq!(legacy_only.auth_mechanisms(), vec!["LOGIN"]);

        let none = reply(&["250-mx.example", "250 AUTHX PLAIN"]);
        assert!(!none.advertises_auth());
        assert!(none.auth_mechanisms().is_empty());
    }

    #[test]
    fn auth_mechanism_selection_prefers_plain_then_login() {
        let both = reply(&["250-x", "250 AUTH LOGIN PLAIN"]);
        assert_eq!(select_auth_mechanism(&both), Ok(AuthMechanism::Plain));
        let bare = reply(&["250-x", "250 AUTH"]);
        assert_eq!(select_auth_mechanism(&bare), Ok(AuthMechanism::Plain));
        let login = reply(&["250-x", "250 AUTH LOGIN CRAM-MD5"]);
        assert_eq!(select_auth_mechanism(&login), Ok(AuthMechanism::Login));
        let legacy_login = reply(&["250-x", "250 AUTH=LOGIN"]);
        assert_eq!(select_auth_mechanism(&legacy_login), Ok(AuthMechanism::Login));
        let oauth_only = reply(&["250-x", "250 AUTH XOAUTH2"]);
        assert_eq!(
            select_auth_mechanism(&oauth_only),
            Err(Some(vec!["XOAUTH2".to_string()]))
        );
        let missing = reply(&["250-x", "250 SIZE 1"]);
        assert_eq!(select_auth_mechanism(&missing), Err(None));
    }

    #[test]
    fn data_write_timeout_scales_and_caps() {
        assert_eq!(data_write_timeout(0), SMTP_STEP_TIMEOUT);
        assert_eq!(data_write_timeout(160 * 1024), Duration::from_secs(40));
        assert_eq!(data_write_timeout(usize::MAX), Duration::from_secs(15 * 60));
    }

    #[test]
    fn smtp_body_normalizes_lines_and_dot_stuffs() {
        assert_eq!(
            encode_smtp_body("one\n.\r\n..three"),
            "one\r\n..\r\n...three\r\n"
        );
        assert_eq!(encode_smtp_body("one\n"), "one\r\n");
    }
}
