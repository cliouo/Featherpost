//! Tunnel stream and connector supporting Direct TCP, HTTP CONNECT, HTTPS CONNECT (TLS to proxy),
//! SOCKS5 (local DNS), and SOCKS5h (remote DNS) with RFC 1928 & RFC 1929 authentication.

use std::fmt;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, ReadBuf};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

use crate::model::{
    format_host_port, normalize_host, ProxyConfig, ProxyEndpoint, ProxyError, ProxyProtocol,
    ProxyTarget, ResolvedRoute, SystemProxySnapshot,
};
use crate::resolve::{resolve_route_with_options, ResolveOptions};

/// Maximum HTTP CONNECT response header size (8 KiB).
pub const MAX_HTTP_CONNECT_HEADER_BYTES: usize = 8 * 1024;

#[cfg(unix)]
const TCP_KEEPALIVE_IDLE_SECS: libc::c_int = 120;

/// Underlying stream returned by [`ProxyConnector::connect`].
pub enum ProxyStream {
    Direct(TcpStream),
    Tls(tokio_native_tls::TlsStream<TcpStream>),
}

impl fmt::Debug for ProxyStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct(_) => f.write_str("ProxyStream::Direct(..)"),
            Self::Tls(_) => f.write_str("ProxyStream::Tls(..)"),
        }
    }
}

impl AsyncRead for ProxyStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Direct(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ProxyStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            Self::Direct(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Direct(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Direct(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

/// Connection timeout and socket tuning options.
#[derive(Clone, Copy, Debug)]
pub struct ConnectOptions {
    pub connect_per_addr: Duration,
    pub connect_total: Duration,
    pub proxy_handshake: Duration,
    pub tcp_nodelay: bool,
    pub tcp_keepalive: bool,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            connect_per_addr: Duration::from_secs(10),
            connect_total: Duration::from_secs(20),
            proxy_handshake: Duration::from_secs(15),
            tcp_nodelay: true,
            tcp_keepalive: false,
        }
    }
}

impl ConnectOptions {
    /// Short budget suitable for `testProxy` diagnostics.
    pub fn test_default() -> Self {
        Self {
            connect_per_addr: Duration::from_secs(5),
            connect_total: Duration::from_secs(10),
            proxy_handshake: Duration::from_secs(10),
            tcp_nodelay: true,
            tcp_keepalive: false,
        }
    }
}

/// Snapshot-backed proxy connector for TCP-based mail protocols (IMAP, SMTP, Probe, Autodiscover).
#[derive(Clone, Debug)]
pub struct ProxyConnector {
    config: ProxyConfig,
    system: SystemProxySnapshot,
    resolve_options: ResolveOptions,
    #[cfg(any(test, debug_assertions))]
    accept_invalid_proxy_certs: bool,
}

impl ProxyConnector {
    pub fn new(config: ProxyConfig, system: SystemProxySnapshot) -> Self {
        Self {
            config,
            system,
            resolve_options: ResolveOptions::default(),
            #[cfg(any(test, debug_assertions))]
            accept_invalid_proxy_certs: false,
        }
    }

    /// Always-direct connector (used when no ProxyService is mounted, e.g. isolated unit tests).
    pub fn direct_only() -> Self {
        Self::new(
            ProxyConfig {
                mode: crate::model::ProxyMode::Direct,
                custom: None,
            },
            SystemProxySnapshot::default(),
        )
    }

    #[cfg(any(test, debug_assertions))]
    pub fn with_allow_loopback_proxy(mut self, allow: bool) -> Self {
        self.resolve_options.allow_loopback_proxy = allow;
        self
    }

    #[cfg(any(test, debug_assertions))]
    pub fn with_accept_invalid_proxy_certs(mut self, accept: bool) -> Self {
        self.accept_invalid_proxy_certs = accept;
        self
    }

    pub fn resolve(&self, target: &ProxyTarget) -> ResolvedRoute {
        resolve_route_with_options(&self.config, &self.system, target, self.resolve_options)
    }

    /// Establish a direct or proxied stream to `target`.
    pub async fn connect(
        &self,
        target: &ProxyTarget,
        opts: ConnectOptions,
    ) -> Result<(ProxyStream, ResolvedRoute), ProxyError> {
        let route = self.resolve(target);
        let stream = match &route {
            ResolvedRoute::Direct => {
                let tcp = connect_tcp_addrs(&target.host, target.port, opts, false).await?;
                ProxyStream::Direct(tcp)
            }
            ResolvedRoute::Proxy(endpoint) => {
                let accept_invalid = {
                    #[cfg(any(test, debug_assertions))]
                    {
                        self.accept_invalid_proxy_certs
                    }
                    #[cfg(not(any(test, debug_assertions)))]
                    {
                        false
                    }
                };
                connect_via_endpoint(endpoint, target, opts, accept_invalid).await?
            }
        };
        Ok((stream, route))
    }
}

/// Connect through a specific [`ProxyEndpoint`] to `target`.
pub async fn connect_via_endpoint(
    endpoint: &ProxyEndpoint,
    target: &ProxyTarget,
    opts: ConnectOptions,
    accept_invalid_proxy_certs: bool,
) -> Result<ProxyStream, ProxyError> {
    let clean_target_host = normalize_host(&target.host).map_err(ProxyError::invalid_config)?;
    if target.port == 0 {
        return Err(ProxyError::invalid_config("目标端口不能为 0"));
    }
    let clean_target = ProxyTarget {
        host: clean_target_host,
        port: target.port,
        scheme: target.scheme,
    };

    let tcp = connect_tcp_addrs(&endpoint.host, endpoint.port, opts, true).await?;
    let handshake_budget = opts.proxy_handshake;

    match endpoint.protocol {
        ProxyProtocol::Http => {
            let mut tcp = tcp;
            tokio::time::timeout(
                handshake_budget,
                http_connect_handshake(&mut tcp, endpoint, &clean_target),
            )
            .await
            .map_err(|_| ProxyError::proxy_timeout("HTTP 代理 CONNECT 握手超时"))??;
            Ok(ProxyStream::Direct(tcp))
        }
        ProxyProtocol::Https => {
            let mut builder = native_tls::TlsConnector::builder();
            #[cfg(any(test, debug_assertions))]
            if accept_invalid_proxy_certs {
                builder.danger_accept_invalid_certs(true);
            }
            #[cfg(not(any(test, debug_assertions)))]
            let _ = accept_invalid_proxy_certs;
            let ntls = builder.build().map_err(|e| {
                ProxyError::proxy_connect_failed("构建 HTTPS 代理 TLS 连接器失败", Some(e.to_string()))
            })?;
            let tls_connector = tokio_native_tls::TlsConnector::from(ntls);

            let deadline = tokio::time::Instant::now() + handshake_budget;
            let mut tls_stream = tokio::time::timeout_at(
                deadline,
                tls_connector.connect(&endpoint.host, tcp),
            )
            .await
            .map_err(|_| ProxyError::proxy_timeout("HTTPS 代理 TLS 握手超时"))?
            .map_err(|e| {
                ProxyError::proxy_connect_failed("HTTPS 代理 TLS 握手失败", Some(e.to_string()))
            })?;

            tokio::time::timeout_at(
                deadline,
                http_connect_handshake(&mut tls_stream, endpoint, &clean_target),
            )
            .await
            .map_err(|_| ProxyError::proxy_timeout("HTTPS 代理 CONNECT 握手超时"))??;
            Ok(ProxyStream::Tls(tls_stream))
        }
        ProxyProtocol::Socks5 | ProxyProtocol::Socks5h => {
            let mut tcp = tcp;
            let remote_dns = endpoint.protocol == ProxyProtocol::Socks5h;
            tokio::time::timeout(
                handshake_budget,
                socks5_handshake(&mut tcp, endpoint, &clean_target, remote_dns),
            )
            .await
            .map_err(|_| ProxyError::proxy_timeout("SOCKS5 代理握手超时"))??;
            Ok(ProxyStream::Direct(tcp))
        }
    }
}

async fn connect_tcp_addrs(
    host: &str,
    port: u16,
    opts: ConnectOptions,
    is_proxy: bool,
) -> Result<TcpStream, ProxyError> {
    let deadline = tokio::time::Instant::now() + opts.connect_total;
    let addrs: Vec<std::net::SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![std::net::SocketAddr::new(ip, port)]
    } else {
        match tokio::time::timeout(opts.connect_total, tokio::net::lookup_host((host, port))).await {
            Ok(Ok(iter)) => iter.collect(),
            Ok(Err(error)) => {
                return Err(if is_proxy {
                    ProxyError::proxy_connect_failed(
                        "代理服务器 DNS 解析失败",
                        Some(error.to_string()),
                    )
                } else {
                    ProxyError::direct_failed("DNS 解析失败", Some(error.to_string()))
                });
            }
            Err(_) => {
                return Err(if is_proxy {
                    ProxyError::proxy_timeout("代理服务器 DNS 解析超时")
                } else {
                    ProxyError::direct_timeout("DNS 解析超时")
                });
            }
        }
    };

    if addrs.is_empty() {
        return Err(if is_proxy {
            ProxyError::proxy_connect_failed("代理服务器无可用地址", None)
        } else {
            ProxyError::direct_failed("没有可用地址", None)
        });
    }

    let mut last_error = if is_proxy {
        ProxyError::proxy_connect_failed("无法连接代理服务器", None)
    } else {
        ProxyError::direct_failed("连接失败", None)
    };

    for addr in addrs {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            last_error = if is_proxy {
                ProxyError::proxy_timeout("连接代理服务器超时")
            } else {
                ProxyError::direct_timeout("连接超时")
            };
            break;
        }
        match tokio::time::timeout(opts.connect_per_addr.min(remaining), TcpStream::connect(addr))
            .await
        {
            Ok(Ok(stream)) => {
                tune_tcp_stream(&stream, opts);
                return Ok(stream);
            }
            Ok(Err(error)) => {
                last_error = if is_proxy {
                    ProxyError::proxy_connect_failed("无法连接代理服务器", Some(error.to_string()))
                } else {
                    ProxyError::direct_failed("连接失败", Some(error.to_string()))
                };
            }
            Err(_) => {
                last_error = if is_proxy {
                    ProxyError::proxy_timeout("连接代理服务器超时")
                } else {
                    ProxyError::direct_timeout(format!("{addr} 连接超时"))
                };
            }
        }
    }
    Err(last_error)
}

fn tune_tcp_stream(stream: &TcpStream, opts: ConnectOptions) {
    if opts.tcp_nodelay {
        let _ = stream.set_nodelay(true);
    }
    #[cfg(unix)]
    if opts.tcp_keepalive {
        use std::os::fd::AsRawFd;
        let fd = stream.as_raw_fd();
        let set = |level: libc::c_int, name: libc::c_int, value: libc::c_int| {
            // SAFETY: fd is valid for the lifetime of `stream`; `value` is a stack `c_int`.
            unsafe {
                libc::setsockopt(
                    fd,
                    level,
                    name,
                    &value as *const libc::c_int as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        };
        set(libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1);
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        set(libc::IPPROTO_TCP, libc::TCP_KEEPALIVE, TCP_KEEPALIVE_IDLE_SECS);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        set(libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, TCP_KEEPALIVE_IDLE_SECS);
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "linux",
            target_os = "android"
        ))]
        {
            set(libc::IPPROTO_TCP, libc::TCP_KEEPINTVL, 30);
            set(libc::IPPROTO_TCP, libc::TCP_KEEPCNT, 4);
        }
    }
}

/// HTTP CONNECT handshake over any stream (plain TCP or TLS).
///
/// Guards against CRLF/whitespace injection and reads response headers byte-by-byte up to
/// `\r\n\r\n` (capped at 8 KiB) with zero `BufReader` over-read.
async fn http_connect_handshake<S>(
    stream: &mut S,
    endpoint: &ProxyEndpoint,
    target: &ProxyTarget,
) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Defense-in-depth injection check before writing HTTP request line.
    if target.host.is_empty()
        || target
            .host
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(ProxyError::invalid_config("目标主机名包含非法字符"));
    }
    let authority = format_host_port(&target.host, target.port);
    let mut request = Zeroizing::new(format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: Keep-Alive\r\n"
    ));

    let user = endpoint.username.as_deref().unwrap_or("");
    let pass = endpoint.password.as_deref().unwrap_or("");
    if !user.is_empty() || !pass.is_empty() {
        if user.chars().any(char::is_control)
            || user.contains(':')
            || pass.chars().any(|c| matches!(c, '\r' | '\n' | '\0'))
        {
            return Err(ProxyError::invalid_config("代理认证凭据包含非法字符"));
        }
        let raw_creds = Zeroizing::new(format!("{user}:{pass}"));
        let encoded = Zeroizing::new(
            base64::engine::general_purpose::STANDARD.encode(raw_creds.as_bytes()),
        );
        request.push_str("Proxy-Authorization: Basic ");
        request.push_str(encoded.as_str());
        request.push_str("\r\n");
    }
    request.push_str("\r\n");

    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| ProxyError::proxy_connect_failed("发送 HTTP CONNECT 请求失败", Some(e.to_string())))?;
    stream
        .flush()
        .await
        .map_err(|e| ProxyError::proxy_connect_failed("刷新 HTTP CONNECT 请求失败", Some(e.to_string())))?;
    drop(request);

    // Byte-by-byte read until `\r\n\r\n` so zero target payload bytes are over-read.
    let mut header_bytes = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await.map_err(|e| {
            ProxyError::tunnel_failed(None, None, "读取 HTTP CONNECT 响应失败", Some(e.to_string()))
        })?;
        if n == 0 {
            return Err(ProxyError::tunnel_failed(
                None,
                None,
                "代理在 HTTP CONNECT 响应结束前关闭连接",
                None,
            ));
        }
        if header_bytes.len() >= MAX_HTTP_CONNECT_HEADER_BYTES {
            return Err(ProxyError::tunnel_failed(
                None,
                None,
                "HTTP CONNECT 响应头超过 8 KiB 上限",
                None,
            ));
        }
        header_bytes.push(byte[0]);
        if header_bytes.ends_with(b"\r\n\r\n") {
            break;
        }
    }

    let status_line_end = header_bytes
        .windows(2)
        .position(|w| w == b"\r\n")
        .unwrap_or(header_bytes.len());
    let status_line = std::str::from_utf8(&header_bytes[..status_line_end]).map_err(|_| {
        ProxyError::tunnel_failed(None, None, "HTTP CONNECT 状态行不是有效的 UTF-8", None)
    })?;

    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    let code_str = parts.next().unwrap_or("");
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(ProxyError::tunnel_failed(
            None,
            None,
            "HTTP CONNECT 响应协议无效",
            None,
        ));
    }
    let code: u16 = code_str.parse().map_err(|_| {
        ProxyError::tunnel_failed(None, None, "HTTP CONNECT 状态码无效", None)
    })?;

    match code {
        200..=299 => Ok(()),
        407 => Err(ProxyError::auth_required(
            Some(407),
            "代理要求身份认证（HTTP 407）",
        )),
        _ => Err(ProxyError::tunnel_failed(
            Some(code),
            None,
            format!("HTTP CONNECT 隧道被拒绝（HTTP {code}）"),
            None,
        )),
    }
}

/// SOCKS5 (RFC 1928) + Username/Password auth (RFC 1929).
async fn socks5_handshake<S>(
    stream: &mut S,
    endpoint: &ProxyEndpoint,
    target: &ProxyTarget,
    remote_dns: bool,
) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let user = endpoint.username.as_deref().unwrap_or("");
    let pass = endpoint.password.as_deref().unwrap_or("");
    let has_auth = !user.is_empty() || !pass.is_empty();

    // 1. Greeting: version 5 + offered methods.
    if has_auth {
        if user.len() > 255 || pass.len() > 255 {
            return Err(ProxyError::invalid_config(
                "SOCKS5 认证用户名或密码超过 255 字节上限",
            ));
        }
        stream
            .write_all(&[0x05, 0x02, 0x00, 0x02])
            .await
            .map_err(|e| ProxyError::proxy_connect_failed("发送 SOCKS5 握手失败", Some(e.to_string())))?;
    } else {
        stream
            .write_all(&[0x05, 0x01, 0x00])
            .await
            .map_err(|e| ProxyError::proxy_connect_failed("发送 SOCKS5 握手失败", Some(e.to_string())))?;
    }
    stream
        .flush()
        .await
        .map_err(|e| ProxyError::proxy_connect_failed("刷新 SOCKS5 握手失败", Some(e.to_string())))?;

    let mut method_reply = [0u8; 2];
    stream.read_exact(&mut method_reply).await.map_err(|e| {
        ProxyError::tunnel_failed(None, None, "读取 SOCKS5 认证协商失败", Some(e.to_string()))
    })?;
    if method_reply[0] != 0x05 {
        return Err(ProxyError::tunnel_failed(
            None,
            None,
            format!("SOCKS5 协议版本无效（0x{:02x}）", method_reply[0]),
            None,
        ));
    }

    match method_reply[1] {
        0x00 => {}
        0x02 => {
            if !has_auth {
                return Err(ProxyError::auth_required(
                    None,
                    "SOCKS5 代理要求用户名和密码认证",
                ));
            }
            // RFC 1929: VER(1) ULEN(1) UNAME PLEN(1) PASSWD
            let mut auth_req = Zeroizing::new(Vec::with_capacity(3 + user.len() + pass.len()));
            auth_req.push(0x01);
            auth_req.push(user.len() as u8);
            auth_req.extend_from_slice(user.as_bytes());
            auth_req.push(pass.len() as u8);
            auth_req.extend_from_slice(pass.as_bytes());
            stream.write_all(&auth_req).await.map_err(|e| {
                ProxyError::proxy_connect_failed("发送 SOCKS5 认证请求失败", Some(e.to_string()))
            })?;
            stream.flush().await.map_err(|e| {
                ProxyError::proxy_connect_failed("刷新 SOCKS5 认证请求失败", Some(e.to_string()))
            })?;
            drop(auth_req);

            let mut auth_reply = [0u8; 2];
            stream.read_exact(&mut auth_reply).await.map_err(|e| {
                ProxyError::tunnel_failed(None, None, "读取 SOCKS5 认证响应失败", Some(e.to_string()))
            })?;
            if auth_reply[0] != 0x01 || auth_reply[1] != 0x00 {
                return Err(ProxyError::auth_required(
                    None,
                    "SOCKS5 代理拒绝了用户名或密码",
                ));
            }
        }
        0xFF => {
            return Err(ProxyError::auth_required(
                None,
                "SOCKS5 代理没有可接受的认证方式（可能需要用户名和密码）",
            ));
        }
        other => {
            return Err(ProxyError::auth_required(
                None,
                format!("SOCKS5 代理选择了不支持的认证方式（0x{other:02x}）"),
            ));
        }
    }

    // 2. CONNECT request: VER(5) CMD(1=CONNECT) RSV(0) ATYP DST.ADDR DST.PORT
    let mut req = Vec::with_capacity(7 + target.host.len());
    req.extend_from_slice(&[0x05, 0x01, 0x00]);
    if let Ok(ip) = target.host.parse::<IpAddr>() {
        push_socks5_ip(&mut req, ip);
    } else if remote_dns {
        if target.host.len() > 255 {
            return Err(ProxyError::invalid_config("SOCKS5h 目标域名超过 255 字节"));
        }
        req.push(0x03);
        req.push(target.host.len() as u8);
        req.extend_from_slice(target.host.as_bytes());
    } else {
        // Local DNS resolution for `socks5` (non-`h`).
        let resolved = tokio::net::lookup_host((target.host.as_str(), target.port))
            .await
            .map_err(|e| {
                ProxyError::tunnel_failed(None, None, "SOCKS5 本地 DNS 解析失败", Some(e.to_string()))
            })?
            .next()
            .ok_or_else(|| {
                ProxyError::tunnel_failed(None, None, "SOCKS5 本地 DNS 无可用地址", None)
            })?;
        push_socks5_ip(&mut req, resolved.ip());
    }
    req.extend_from_slice(&target.port.to_be_bytes());

    stream
        .write_all(&req)
        .await
        .map_err(|e| ProxyError::tunnel_failed(None, None, "发送 SOCKS5 CONNECT 失败", Some(e.to_string())))?;
    stream
        .flush()
        .await
        .map_err(|e| ProxyError::tunnel_failed(None, None, "刷新 SOCKS5 CONNECT 失败", Some(e.to_string())))?;

    // 3. CONNECT reply: VER(5) REP RSV ATYP BND.ADDR BND.PORT
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await.map_err(|e| {
        ProxyError::tunnel_failed(None, None, "读取 SOCKS5 CONNECT 响应失败", Some(e.to_string()))
    })?;
    if header[0] != 0x05 {
        return Err(ProxyError::tunnel_failed(
            None,
            None,
            format!("SOCKS5 响应版本无效（0x{:02x}）", header[0]),
            None,
        ));
    }
    let rep = header[1];
    let atyp = header[3];

    // Drain BND.ADDR + BND.PORT completely before returning so no trailing protocol bytes remain.
    match atyp {
        0x01 => {
            let mut rest = [0u8; 6];
            stream.read_exact(&mut rest).await.map_err(|e| {
                ProxyError::tunnel_failed(None, Some(rep), "读取 SOCKS5 IPv4 绑定地址失败", Some(e.to_string()))
            })?;
        }
        0x04 => {
            let mut rest = [0u8; 18];
            stream.read_exact(&mut rest).await.map_err(|e| {
                ProxyError::tunnel_failed(None, Some(rep), "读取 SOCKS5 IPv6 绑定地址失败", Some(e.to_string()))
            })?;
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await.map_err(|e| {
                ProxyError::tunnel_failed(None, Some(rep), "读取 SOCKS5 域名绑定长度失败", Some(e.to_string()))
            })?;
            let mut rest = vec![0u8; len[0] as usize + 2];
            stream.read_exact(&mut rest).await.map_err(|e| {
                ProxyError::tunnel_failed(None, Some(rep), "读取 SOCKS5 域名绑定地址失败", Some(e.to_string()))
            })?;
        }
        other => {
            return Err(ProxyError::tunnel_failed(
                None,
                Some(rep),
                format!("SOCKS5 响应包含未知地址类型（0x{other:02x}）"),
                None,
            ));
        }
    }

    if rep == 0x00 {
        return Ok(());
    }
    Err(ProxyError::tunnel_failed(
        None,
        Some(rep),
        format!("SOCKS5 连接目标失败: {}", socks5_rep_description(rep)),
        None,
    ))
}

fn push_socks5_ip(buf: &mut Vec<u8>, ip: IpAddr) {
    match ip {
        IpAddr::V4(v4) => {
            buf.push(0x01);
            buf.extend_from_slice(&v4.octets());
        }
        IpAddr::V6(v6) => {
            buf.push(0x04);
            buf.extend_from_slice(&v6.octets());
        }
    }
}

fn socks5_rep_description(rep: u8) -> &'static str {
    match rep {
        0x01 => "general SOCKS server failure (0x01)",
        0x02 => "connection not allowed by ruleset (0x02)",
        0x03 => "network unreachable (0x03)",
        0x04 => "host unreachable (0x04)",
        0x05 => "connection refused (0x05)",
        0x06 => "TTL expired (0x06)",
        0x07 => "command not supported (0x07)",
        0x08 => "address type not supported (0x08)",
        _ => "unknown SOCKS5 error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProxyErrorKind, TargetScheme};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn http_connect_does_not_overread_trailing_banner() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = vec![0u8; 512];
            let n = sock.read(&mut req).await.unwrap();
            let text = std::str::from_utf8(&req[..n]).unwrap();
            assert!(text.starts_with("CONNECT imap.example.com:993 HTTP/1.1\r\n"));
            assert!(text.contains("Proxy-Authorization: Basic YWxpY2U6c2VjcmV0\r\n"));
            // Send HTTP 200 header + immediate target banner in the same write!
            sock.write_all(b"HTTP/1.1 200 Connection Established\r\nX-Test: ok\r\n\r\n* OK IMAP READY\r\n")
                .await
                .unwrap();
        });

        let ep = ProxyEndpoint {
            protocol: ProxyProtocol::Http,
            host: "127.0.0.1".into(),
            port: addr.port(),
            username: Some("alice".into()),
            password: Some("secret".into()),
        };
        let target = ProxyTarget::new("imap.example.com", 993, TargetScheme::Imaps);
        let mut stream = connect_via_endpoint(&ep, &target, ConnectOptions::test_default(), false)
            .await
            .unwrap();
        let mut banner = [0u8; 17];
        stream.read_exact(&mut banner).await.unwrap();
        assert_eq!(&banner, b"* OK IMAP READY\r\n");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_connect_maps_407_and_rejects_oversized_headers_and_injection() {
        // CRLF injection guard
        let ep = ProxyEndpoint {
            protocol: ProxyProtocol::Http,
            host: "127.0.0.1".into(),
            port: 1,
            username: None,
            password: None,
        };
        let injected = ProxyTarget::new("evil.com\r\nX-Evil: 1", 993, TargetScheme::Imaps);
        let err = connect_via_endpoint(&ep, &injected, ConnectOptions::test_default(), false)
            .await
            .unwrap_err();
        assert_eq!(err.kind, ProxyErrorKind::InvalidConfig);

        // 407 -> AuthRequired
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 256];
            let _ = sock.read(&mut buf).await;
            sock.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
        });
        let ep407 = ProxyEndpoint {
            protocol: ProxyProtocol::Http,
            host: "127.0.0.1".into(),
            port: addr.port(),
            username: None,
            password: None,
        };
        let err = connect_via_endpoint(
            &ep407,
            &ProxyTarget::new("imap.example.com", 993, TargetScheme::Imaps),
            ConnectOptions::test_default(),
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, ProxyErrorKind::AuthRequired);
        assert_eq!(err.status_code, Some(407));
        server.await.unwrap();

        // > 8 KiB response headers rejected
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 256];
            let _ = sock.read(&mut buf).await;
            let mut big = b"HTTP/1.1 200 OK\r\nX-Pad: ".to_vec();
            big.extend(std::iter::repeat_n(b'A', MAX_HTTP_CONNECT_HEADER_BYTES + 64));
            big.extend_from_slice(b"\r\n\r\n");
            let _ = sock.write_all(&big).await;
        });
        let ep_big = ProxyEndpoint {
            protocol: ProxyProtocol::Http,
            host: "127.0.0.1".into(),
            port: addr.port(),
            username: None,
            password: None,
        };
        let err = connect_via_endpoint(
            &ep_big,
            &ProxyTarget::new("imap.example.com", 993, TargetScheme::Imaps),
            ConnectOptions::test_default(),
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, ProxyErrorKind::TunnelFailed);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn socks5h_with_rfc1929_auth_and_domain_bnd_addr() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut greet = [0u8; 4];
            sock.read_exact(&mut greet).await.unwrap();
            assert_eq!(greet, [0x05, 0x02, 0x00, 0x02]);
            sock.write_all(&[0x05, 0x02]).await.unwrap();

            // RFC 1929 auth
            let mut auth = [0u8; 13]; // 1 + 1 + 4("user") + 1 + 6("pw1234")
            sock.read_exact(&mut auth).await.unwrap();
            assert_eq!(&auth, b"\x01\x04user\x06pw1234");
            sock.write_all(&[0x01, 0x00]).await.unwrap();

            // CONNECT with ATYP=0x03 (domain)
            let mut hdr = [0u8; 5];
            sock.read_exact(&mut hdr).await.unwrap();
            assert_eq!(&hdr[..4], &[0x05, 0x01, 0x00, 0x03]);
            let dlen = hdr[4] as usize;
            let mut body = vec![0u8; dlen + 2];
            sock.read_exact(&mut body).await.unwrap();
            assert_eq!(&body[..dlen], b"imap.remote.test");
            assert_eq!(u16::from_be_bytes([body[dlen], body[dlen + 1]]), 993);

            // Reply with domain BND.ADDR + immediate target payload
            let mut reply = vec![0x05, 0x00, 0x00, 0x03, 0x04];
            reply.extend_from_slice(b"bind");
            reply.extend_from_slice(&993u16.to_be_bytes());
            reply.extend_from_slice(b"PAYLOAD");
            sock.write_all(&reply).await.unwrap();
        });

        let ep = ProxyEndpoint {
            protocol: ProxyProtocol::Socks5h,
            host: "127.0.0.1".into(),
            port: addr.port(),
            username: Some("user".into()),
            password: Some("pw1234".into()),
        };
        let mut stream = connect_via_endpoint(
            &ep,
            &ProxyTarget::new("imap.remote.test", 993, TargetScheme::Imaps),
            ConnectOptions::test_default(),
            false,
        )
        .await
        .unwrap();
        let mut payload = [0u8; 7];
        stream.read_exact(&mut payload).await.unwrap();
        assert_eq!(&payload, b"PAYLOAD");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn socks5_maps_auth_rejection_and_rep_errors() {
        // Bad password
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut greet = [0u8; 4];
            sock.read_exact(&mut greet).await.unwrap();
            sock.write_all(&[0x05, 0x02]).await.unwrap();
            let mut auth = [0u8; 9];
            sock.read_exact(&mut auth).await.unwrap();
            sock.write_all(&[0x01, 0x01]).await.unwrap();
        });
        let ep = ProxyEndpoint {
            protocol: ProxyProtocol::Socks5h,
            host: "127.0.0.1".into(),
            port: addr.port(),
            username: Some("bad".into()),
            password: Some("pwd".into()),
        };
        let err = connect_via_endpoint(
            &ep,
            &ProxyTarget::new("imap.remote.test", 993, TargetScheme::Imaps),
            ConnectOptions::test_default(),
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, ProxyErrorKind::AuthRequired);
        assert!(!err.to_string().contains("pwd"));
        server.await.unwrap();

        // REP = 0x05 (connection refused)
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut greet = [0u8; 3];
            sock.read_exact(&mut greet).await.unwrap();
            sock.write_all(&[0x05, 0x00]).await.unwrap();
            let mut req = [0u8; 10]; // VER CMD RSV ATYP(1) IPv4(4) PORT(2)
            sock.read_exact(&mut req).await.unwrap();
            assert_eq!(req[3], 0x01);
            sock.write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        });
        let ep = ProxyEndpoint {
            protocol: ProxyProtocol::Socks5,
            host: "127.0.0.1".into(),
            port: addr.port(),
            username: None,
            password: None,
        };
        let err = connect_via_endpoint(
            &ep,
            &ProxyTarget::new("127.0.0.1", 993, TargetScheme::Imaps),
            ConnectOptions::test_default(),
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, ProxyErrorKind::TunnelFailed);
        assert_eq!(err.socks_rep, Some(0x05));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn https_proxy_establishes_tls_then_http_connect() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certified = rcgen::generate_simple_self_signed(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
        ])
        .unwrap();
        let cert_der = certified.cert.der().clone();
        let key_der =
            rustls::pki_types::PrivateKeyDer::try_from(certified.signing_key.serialize_der())
                .unwrap();
        let server_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server_cfg));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut tls = acceptor.accept(tcp).await.unwrap();
            let mut buf = vec![0u8; 512];
            let n = tls.read(&mut buf).await.unwrap();
            let text = std::str::from_utf8(&buf[..n]).unwrap();
            assert!(text.starts_with("CONNECT smtp.example.com:465 HTTP/1.1\r\n"));
            tls.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n220 SMTP READY\r\n")
                .await
                .unwrap();
            tls.flush().await.unwrap();
        });

        let ep = ProxyEndpoint {
            protocol: ProxyProtocol::Https,
            host: "127.0.0.1".into(),
            port: addr.port(),
            username: None,
            password: None,
        };
        let mut stream = connect_via_endpoint(
            &ep,
            &ProxyTarget::new("smtp.example.com", 465, TargetScheme::Smtps),
            ConnectOptions::test_default(),
            true,
        )
        .await
        .unwrap();
        assert!(matches!(stream, ProxyStream::Tls(_)));
        let mut banner = [0u8; 16];
        stream.read_exact(&mut banner).await.unwrap();
        assert_eq!(&banner, b"220 SMTP READY\r\n");
        server.await.unwrap();
    }
}
