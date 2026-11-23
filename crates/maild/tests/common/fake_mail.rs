//! R4 e2e 公共模块：最小但协议正确的本地假邮件服务器。
//!
//! 背景：Mailpit 没有 IMAP 支持（其 changelog 全文无 imap 提及，实测也无 1143 端口），
//! 故 e2e 自带一个本地假服务器：SMTP 收信（DATA）+ IMAP 读信（LOGIN / SELECT /
//! UID FETCH BODY.PEEK[] / IDLE 推送 EXISTS / NOOP / LOGOUT / STARTTLS），
//! 监听 ephemeral 端口，完全 hermetic，不依赖任何外部进程。
//!
//! 说明：IMAP 侧严格按 RFC 3501 / 2177 / 2595 的帧格式实现（tagged 响应、literal {N}、
//! IDLE continuation、STARTTLS 就地升级），只覆盖本客户端用到的命令子集；
//! 对真实服务器的兼容性验证（Dovecot/Gmail）留作后续（见 docs/PROGRESS.md 开放问题）。
//!
//! SMTP 侧支持明文 / STARTTLS / 隐式 TLS（465 风格：握手完成后才发 `220` greeting，
//! RFC 8314 §3.3），以及可配置的 AUTH 机制宣告（PLAIN / LOGIN / `AUTH=` 旧式写法）与
//! 凭据校验（ADR-0042）。
//!
//! TLS：SMTP/IMAP 共用 rustls + rcgen 自签证书（SAN localhost/127.0.0.1）。STARTTLS 协商阶段
//! 逐字节读（OK 之后紧跟 TLS ClientHello，缓冲读会吞字节）；协商完成后的主循环
//! 用 split + BufReader（读写分离，IDLE 推送不会被取消）。

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use std::collections::BTreeMap;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, watch};

pub struct FakeMail {
    #[allow(dead_code)] // 各测试 crate 使用面不同
    pub smtp_addr: SocketAddr,
    #[allow(dead_code)] // 只测 SMTP 的 crate 不读 IMAP 地址
    pub imap_addr: SocketAddr,
    state: Arc<State>,
    shutdown: watch::Sender<bool>,
}

/// SMTP 侧的 TLS 模式。
#[allow(dead_code)] // 各测试 crate 只用到子集；公共 fixture 保持完整
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SmtpTlsMode {
    /// 明文（`220` → EHLO …）。
    Plain,
    /// 明文 greeting → EHLO 宣告 STARTTLS → 就地 TLS 握手 → 再 EHLO。
    StartTls,
    /// accept 后立即 TLS 握手（465 风格），握手完成后才发 `220` greeting。
    Implicit,
}

/// SMTP EHLO 宣告的 AUTH 机制与服务端接受的机制。
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SmtpAuthMode {
    /// `250-AUTH PLAIN`（既有行为）。
    Plain,
    /// 只宣告 `250-AUTH LOGIN`，拒绝 `AUTH PLAIN`（504）。
    LoginOnly,
    /// 只宣告旧式 `250-AUTH=LOGIN`，拒绝 `AUTH PLAIN`（504）。
    LegacyLoginOnly,
    /// 同时宣告 `250-AUTH PLAIN LOGIN` 与 `250-AUTH=PLAIN LOGIN`，两种都接受。
    PlainAndLogin,
}

impl SmtpAuthMode {
    fn ehlo_line(self) -> &'static [u8] {
        match self {
            SmtpAuthMode::Plain => b"250-AUTH PLAIN\r\n",
            SmtpAuthMode::LoginOnly => b"250-AUTH LOGIN\r\n",
            SmtpAuthMode::LegacyLoginOnly => b"250-AUTH=LOGIN\r\n",
            SmtpAuthMode::PlainAndLogin => b"250-AUTH PLAIN LOGIN\r\n250-AUTH=PLAIN LOGIN\r\n",
        }
    }

    fn accepts_plain(self) -> bool {
        matches!(self, SmtpAuthMode::Plain | SmtpAuthMode::PlainAndLogin)
    }

    fn accepts_login(self) -> bool {
        !matches!(self, SmtpAuthMode::Plain)
    }
}

/// IMAP 侧的 TLS 模式。
#[allow(dead_code)] // 各测试 crate 只用到子集；公共 fixture 保持完整
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ImapTlsMode {
    /// 明文 greeting → STARTTLS 命令 → 就地 TLS 握手。
    StartTls,
    /// accept 后立即 TLS 握手（993 风格），随后才发 greeting。
    Implicit,
}

struct State {
    messages: Mutex<Vec<RawMessage>>,
    next_uid: AtomicU64,
    imap_fetches: AtomicUsize,
    /// 已接受的 IMAP 连接数——连接预算的验收靠它。
    imap_sessions: AtomicUsize,
    notify: broadcast::Sender<usize>,
    /// Some(密码) 时 LOGIN 必须匹配；None 接受任意凭据（既有测试行为）。
    imap_password: Mutex<Option<String>>,
    /// INBOX 的 UIDVALIDITY，可在运行中翻转（M4.5-02 重建路径）。
    inbox_uid_validity: AtomicU64,
    /// INBOX 之外的 mailbox（M4.5-02 多文件夹）。
    extra: Mutex<BTreeMap<String, MailboxState>>,
    /// 每条 `UID FETCH` 的 (mailbox, range, items)，用于区分正文抓取与标记对账。
    fetch_log: Mutex<Vec<(String, String, String)>>,
    /// 每条写命令的原文（`UID STORE` / `UID COPY` / `UID MOVE` / `UID EXPUNGE`）。
    command_log: Mutex<Vec<String>>,
    /// greeting 与 `CAPABILITY` 宣告的能力串。
    capabilities: Mutex<String>,
    /// SMTP AUTH 宣告与接受的机制。
    smtp_auth: SmtpAuthMode,
    /// Some((用户名, 密码)) 时 SMTP AUTH 必须匹配，且未认证的 MAIL FROM 返回 530；
    /// None 接受任意凭据（既有测试行为）。
    smtp_credentials: Option<(String, String)>,
    /// 每次 SMTP AUTH 的 (机制, 是否成功)，断言协商结果用。
    smtp_auth_log: Mutex<Vec<(String, bool)>>,
}

#[derive(Default)]
struct MailboxState {
    /// LIST 回复里的属性串，例如 `\\Sent`。
    attributes: String,
    uid_validity: u64,
    next_uid: u64,
    messages: Vec<RawMessage>,
}

struct RawMessage {
    uid: u64,
    /// FETCH 回复里的 FLAGS 内容（空串 = `FLAGS ()`）。
    flags: String,
    /// FETCH 回复里的 `INTERNALDATE`（`None` 模拟不返回 INTERNALDATE 的旧服务器）。
    internal_date: Option<String>,
    raw: Vec<u8>,
}

fn format_imap_internal_date(unix_secs: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = (unix_secs / 86_400) as i64;
    let seconds_of_day = unix_secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!(
        "{d:02}-{}-{year:04} {:02}:{:02}:{:02} +0000",
        MONTHS[(m - 1) as usize],
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    )
}

fn now_imap_internal_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_imap_internal_date(secs)
}

impl State {
    fn smtp_credentials_match(&self, user: &[u8], pass: &[u8]) -> bool {
        match &self.smtp_credentials {
            None => true,
            Some((expected_user, expected_pass)) => {
                user == expected_user.as_bytes() && pass == expected_pass.as_bytes()
            }
        }
    }

    fn log_smtp_auth(&self, mechanism: &str, ok: bool) {
        self.smtp_auth_log
            .lock()
            .unwrap()
            .push((mechanism.to_string(), ok));
    }

    fn count(&self) -> usize {
        self.messages.lock().unwrap().len()
    }

    fn deliver(&self, raw: Vec<u8>) -> usize {
        let uid = self.next_uid.fetch_add(1, Ordering::Relaxed);
        self.messages.lock().unwrap().push(RawMessage {
            uid,
            flags: String::new(),
            internal_date: Some(now_imap_internal_date()),
            raw,
        });
        let n = self.count();
        let _ = self.notify.send(n);
        n
    }

    fn is_inbox(name: &str) -> bool {
        name.eq_ignore_ascii_case("INBOX")
    }

    fn mailbox_count(&self, name: &str) -> usize {
        if Self::is_inbox(name) {
            return self.count();
        }
        self.extra
            .lock()
            .unwrap()
            .get(name)
            .map(|mailbox| mailbox.messages.len())
            .unwrap_or(0)
    }

    fn mailbox_uid_validity(&self, name: &str) -> u64 {
        if Self::is_inbox(name) {
            return self.inbox_uid_validity.load(Ordering::Relaxed);
        }
        self.extra
            .lock()
            .unwrap()
            .get(name)
            .map(|mailbox| mailbox.uid_validity)
            .unwrap_or(1)
    }

    fn mailbox_next_uid(&self, name: &str) -> u64 {
        if Self::is_inbox(name) {
            return self.next_uid.load(Ordering::Relaxed);
        }
        self.extra
            .lock()
            .unwrap()
            .get(name)
            .map(|mailbox| mailbox.next_uid)
            .unwrap_or(1)
    }

    /// (uid, flags, internal_date, raw)，按 uid 升序。
    fn mailbox_snapshot(&self, name: &str) -> Vec<(u64, String, Option<String>, Vec<u8>)> {
        if Self::is_inbox(name) {
            return self
                .messages
                .lock()
                .unwrap()
                .iter()
                .map(|message| {
                    (
                        message.uid,
                        message.flags.clone(),
                        message.internal_date.clone(),
                        message.raw.clone(),
                    )
                })
                .collect();
        }
        self.extra
            .lock()
            .unwrap()
            .get(name)
            .map(|mailbox| {
                mailbox
                    .messages
                    .iter()
                    .map(|message| {
                        (
                            message.uid,
                            message.flags.clone(),
                            message.internal_date.clone(),
                            message.raw.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn set_flag(&self, mailbox: &str, uid: u64, flag: &str, on: bool) {
        let apply = |message: &mut RawMessage| {
            let mut flags: Vec<String> = message
                .flags
                .split_whitespace()
                .map(|value| value.to_string())
                .collect();
            flags.retain(|value| !value.eq_ignore_ascii_case(flag));
            if on {
                flags.push(flag.to_string());
            }
            message.flags = flags.join(" ");
        };
        if Self::is_inbox(mailbox) {
            if let Some(message) = self
                .messages
                .lock()
                .unwrap()
                .iter_mut()
                .find(|message| message.uid == uid)
            {
                apply(message);
            }
            return;
        }
        if let Some(entry) = self.extra.lock().unwrap().get_mut(mailbox) {
            if let Some(message) = entry.messages.iter_mut().find(|message| message.uid == uid) {
                apply(message);
            }
        }
    }

    fn take_raw(&self, mailbox: &str, uid: u64) -> Option<(String, Option<String>, Vec<u8>)> {
        self.mailbox_snapshot(mailbox)
            .into_iter()
            .find(|(existing, _, _, _)| *existing == uid)
            .map(|(_, flags, internal_date, raw)| (flags, internal_date, raw))
    }

    fn append(&self, mailbox: &str, flags: &str, internal_date: Option<String>, raw: Vec<u8>) {
        if Self::is_inbox(mailbox) {
            let uid = self.next_uid.fetch_add(1, Ordering::Relaxed);
            self.messages.lock().unwrap().push(RawMessage {
                uid,
                flags: flags.to_string(),
                internal_date,
                raw,
            });
            return;
        }
        let mut extra = self.extra.lock().unwrap();
        let entry = extra.entry(mailbox.to_string()).or_insert_with(|| MailboxState {
            attributes: String::new(),
            uid_validity: 1,
            next_uid: 1,
            messages: Vec::new(),
        });
        let uid = entry.next_uid;
        entry.next_uid += 1;
        entry.messages.push(RawMessage {
            uid,
            flags: flags.to_string(),
            internal_date,
            raw,
        });
    }

    fn remove(&self, mailbox: &str, uid: u64) {
        if Self::is_inbox(mailbox) {
            self.messages.lock().unwrap().retain(|message| message.uid != uid);
            return;
        }
        if let Some(entry) = self.extra.lock().unwrap().get_mut(mailbox) {
            entry.messages.retain(|message| message.uid != uid);
        }
    }

    /// LIST 回复用的 (name, attributes)，INBOX 在前。
    fn mailbox_listing(&self) -> Vec<(String, String)> {
        let mut out = vec![("INBOX".to_string(), String::new())];
        for (name, mailbox) in self.extra.lock().unwrap().iter() {
            out.push((name.clone(), mailbox.attributes.clone()));
        }
        out
    }
}

#[allow(dead_code)] // 同上：按测试 crate 裁剪使用面
impl FakeMail {
    /// 明文 IMAP（原行为）。
    pub async fn start() -> io::Result<FakeMail> {
        Self::start_with(None, false).await
    }

    /// 带 TLS 的 IMAP（STARTTLS 或隐式 TLS），SMTP 侧仍明文。
    pub async fn start_tls(mode: ImapTlsMode) -> io::Result<FakeMail> {
        Self::start_with(Some(mode), false).await
    }

    /// SMTP 侧声明 STARTTLS 并用自签证书升级；IMAP 侧保持明文。
    pub async fn start_smtp_starttls() -> io::Result<FakeMail> {
        Self::start_with(None, true).await
    }

    /// IMAP 隐式 TLS + SMTP STARTTLS：受管账户全链路（M3.7-01 e2e）用。
    pub async fn start_imap_implicit_and_smtp_starttls() -> io::Result<FakeMail> {
        Self::start_with(Some(ImapTlsMode::Implicit), true).await
    }

    async fn start_with(
        imap_tls: Option<ImapTlsMode>,
        smtp_starttls: bool,
    ) -> io::Result<FakeMail> {
        Self::start_with_options(imap_tls, smtp_starttls, None).await
    }

    /// 只关心 SMTP 侧的假服务器（IMAP 保持明文）：可选隐式 TLS（465 风格）、
    /// AUTH 机制宣告与凭据校验。`credentials = None` 时接受任意凭据。
    pub async fn start_smtp(
        tls: SmtpTlsMode,
        auth: SmtpAuthMode,
        credentials: Option<(&str, &str)>,
    ) -> io::Result<FakeMail> {
        Self::start_full(
            None,
            tls,
            None,
            auth,
            credentials.map(|(user, pass)| (user.to_string(), pass.to_string())),
        )
        .await
    }

    /// SMTP 隐式 TLS（465 风格）+ 宣告 `AUTH PLAIN`，接受任意凭据。
    pub async fn start_smtp_implicit() -> io::Result<FakeMail> {
        Self::start_smtp(SmtpTlsMode::Implicit, SmtpAuthMode::Plain, None).await
    }

    /// IMAP 隐式 TLS（993 风格）+ SMTP 隐式 TLS（465 风格），两侧都校验同一组凭据：
    /// 真实 provider 的全 TLS 形态（`footprint_bench` 的多账户回环压测用）。
    pub async fn start_all_implicit_tls(
        auth: SmtpAuthMode,
        username: &str,
        password: &str,
    ) -> io::Result<FakeMail> {
        Self::start_full(
            Some(ImapTlsMode::Implicit),
            SmtpTlsMode::Implicit,
            Some(password.to_string()),
            auth,
            Some((username.to_string(), password.to_string())),
        )
        .await
    }

    /// 每次 SMTP AUTH 的 (机制, 是否成功)。
    pub fn smtp_auth_log(&self) -> Vec<(String, bool)> {
        self.state.smtp_auth_log.lock().unwrap().clone()
    }

    /// IMAP 隐式 TLS + SMTP STARTTLS，且 LOGIN 校验指定密码（M3.7 恢复矩阵用）。
    pub async fn start_imap_implicit_and_smtp_starttls_with_login(
        password: &str,
    ) -> io::Result<FakeMail> {
        Self::start_with_options(Some(ImapTlsMode::Implicit), true, Some(password.to_string()))
            .await
    }

    /// 服务端侧撤销/更换密码（模拟用户在 provider 处改密）。
    pub fn rotate_imap_password(&self, new_password: &str) {
        *self
            .state
            .imap_password
            .lock()
            .unwrap() = Some(new_password.to_string());
    }

    async fn start_with_options(
        imap_tls: Option<ImapTlsMode>,
        smtp_starttls: bool,
        imap_password: Option<String>,
    ) -> io::Result<FakeMail> {
        let smtp_tls = if smtp_starttls {
            SmtpTlsMode::StartTls
        } else {
            SmtpTlsMode::Plain
        };
        Self::start_full(imap_tls, smtp_tls, imap_password, SmtpAuthMode::Plain, None).await
    }

    async fn start_full(
        imap_tls: Option<ImapTlsMode>,
        smtp_tls: SmtpTlsMode,
        imap_password: Option<String>,
        smtp_auth: SmtpAuthMode,
        smtp_credentials: Option<(String, String)>,
    ) -> io::Result<FakeMail> {
        let state = Arc::new(State {
            messages: Mutex::new(Vec::new()),
            next_uid: AtomicU64::new(1),
            imap_fetches: AtomicUsize::new(0),
            imap_sessions: AtomicUsize::new(0),
            notify: broadcast::channel(64).0,
            imap_password: Mutex::new(imap_password),
            inbox_uid_validity: AtomicU64::new(1),
            extra: Mutex::new(BTreeMap::new()),
            fetch_log: Mutex::new(Vec::new()),
            command_log: Mutex::new(Vec::new()),
            capabilities: Mutex::new("IMAP4rev1 IDLE".to_string()),
            smtp_auth,
            smtp_credentials,
            smtp_auth_log: Mutex::new(Vec::new()),
        });
        let acceptor = if imap_tls.is_some() || smtp_tls != SmtpTlsMode::Plain {
            Some(Arc::new(make_tls_acceptor()?))
        } else {
            None
        };
        let smtp = TcpListener::bind("127.0.0.1:0").await?;
        let imap = TcpListener::bind("127.0.0.1:0").await?;
        let smtp_addr = smtp.local_addr()?;
        let imap_addr = imap.local_addr()?;
        let (shutdown, shutdown_rx) = watch::channel(false);

        {
            let state = Arc::clone(&state);
            let acceptor = acceptor.clone();
            let mut rx = shutdown_rx.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = rx.changed() => break,
                        accepted = smtp.accept() => {
                            let (s, _) = accepted?;
                            let st = Arc::clone(&state);
                            let acc = acceptor.clone();
                            tokio::spawn(async move {
                                smtp_session(s, st, smtp_tls, acc).await
                            });
                        }
                    }
                }
                Ok::<(), io::Error>(())
            });
        }
        {
            let state = Arc::clone(&state);
            let acceptor = acceptor.clone();
            let mut rx = shutdown_rx.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = rx.changed() => break,
                        accepted = imap.accept() => {
                            let (s, _) = accepted?;
                            state.imap_sessions.fetch_add(1, Ordering::Relaxed);
                            let st = Arc::clone(&state);
                            let acc = acceptor.clone();
                            tokio::spawn(async move { imap_session(s, st, imap_tls, acc).await });
                        }
                    }
                }
                Ok::<(), io::Error>(())
            });
        }
        drop(shutdown_rx);

        Ok(FakeMail {
            smtp_addr,
            imap_addr,
            state,
            shutdown,
        })
    }

    /// 收到的原始邮件（transport e2e 断言用）。
    pub fn messages(&self) -> Vec<Vec<u8>> {
        self.state
            .messages
            .lock()
            .unwrap()
            .iter()
            .map(|m| m.raw.clone())
            .collect()
    }

    /// 登记一个 INBOX 之外的 mailbox。`attributes` 是 LIST 回复里的属性串，
    /// 例如 `"\\Sent"`（RFC 6154 SPECIAL-USE）；空串表示无特殊用途。
    pub fn add_mailbox(&self, name: &str, attributes: &str) {
        self.state.extra.lock().unwrap().insert(
            name.to_string(),
            MailboxState {
                attributes: attributes.to_string(),
                uid_validity: 1,
                next_uid: 1,
                messages: Vec::new(),
            },
        );
    }

    pub fn remove_mailbox(&self, name: &str) {
        self.state.extra.lock().unwrap().remove(name).unwrap();
    }

    /// 往指定 mailbox 投递一封信；`flags` 是 FETCH 回复里的 FLAGS 内容（可为空串）。
    pub fn deliver_to(&self, mailbox: &str, flags: &str, raw: Vec<u8>) {
        let internal_date = now_imap_internal_date();
        self.deliver_to_with_internal_date(mailbox, flags, Some(&internal_date), raw);
    }

    /// 往指定 mailbox 投递一封带显式 `INTERNALDATE`（或 `None` 省略）的信，供历史时间锚定测试用。
    pub fn deliver_to_with_internal_date(
        &self,
        mailbox: &str,
        flags: &str,
        internal_date: Option<&str>,
        raw: Vec<u8>,
    ) {
        let internal_date = internal_date.map(str::to_string);
        if State::is_inbox(mailbox) {
            let uid = self.state.next_uid.fetch_add(1, Ordering::Relaxed);
            self.state.messages.lock().unwrap().push(RawMessage {
                uid,
                flags: flags.to_string(),
                internal_date,
                raw,
            });
            let n = self.state.count();
            let _ = self.state.notify.send(n);
            return;
        }
        let mut extra = self.state.extra.lock().unwrap();
        let entry = extra.entry(mailbox.to_string()).or_insert_with(|| MailboxState {
            attributes: String::new(),
            uid_validity: 1,
            next_uid: 1,
            messages: Vec::new(),
        });
        let uid = entry.next_uid;
        entry.next_uid += 1;
        entry.messages.push(RawMessage {
            uid,
            flags: flags.to_string(),
            internal_date,
            raw,
        });
    }

    /// 翻转某个 mailbox 的 `UIDVALIDITY`（模拟服务器侧 UID 空间重编号）。
    pub fn set_uid_validity(&self, mailbox: &str, value: u64) {
        if State::is_inbox(mailbox) {
            self.state.inbox_uid_validity.store(value, Ordering::Relaxed);
            return;
        }
        if let Some(entry) = self.state.extra.lock().unwrap().get_mut(mailbox) {
            entry.uid_validity = value;
        }
    }

    /// 收到过的每条 `UID FETCH` 的 (mailbox, range, items)。断言增量抓取用。
    pub fn fetch_log(&self) -> Vec<(String, String, String)> {
        self.state.fetch_log.lock().unwrap().clone()
    }

    /// 收到过的写命令原文。断言 MOVE / COPY 降级链用。
    pub fn command_log(&self) -> Vec<String> {
        self.state.command_log.lock().unwrap().clone()
    }

    /// 覆盖宣告的能力串，例如 `"IMAP4rev1 IDLE MOVE UIDPLUS"`。
    pub fn set_capabilities(&self, value: &str) {
        *self.state.capabilities.lock().unwrap() = value.to_string();
    }

    /// 某个 mailbox 里的 (uid, flags, subject-ish) 快照，供断言写回结果。
    pub fn mailbox_uids(&self, mailbox: &str) -> Vec<(u64, String)> {
        self.state
            .mailbox_snapshot(mailbox)
            .into_iter()
            .map(|(uid, flags, _, _)| (uid, flags))
            .collect()
    }

    pub fn imap_fetches(&self) -> usize {
        self.state.imap_fetches.load(Ordering::Relaxed)
    }

    pub fn imap_connections(&self) -> usize {
        self.state.imap_sessions.load(Ordering::Relaxed)
    }
}

impl Drop for FakeMail {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// rcgen 自签证书（SAN：localhost / 127.0.0.1）→ rustls acceptor（ring provider）。
fn make_tls_acceptor() -> io::Result<tokio_rustls::TlsAcceptor> {
    use rustls::pki_types::PrivatePkcs8KeyDer;
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
        .map_err(|e| io::Error::other(format!("生成自签证书: {e}")))?;
    let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into();
    let cert_der = cert.cert.der().clone();
    // tokio-rustls 关默认特性后无 process-default provider：显式 ring
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| io::Error::other(format!("rustls 版本配置: {e}")))?
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key)
        .map_err(|e| io::Error::other(format!("rustls 配置: {e}")))?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

// —— SMTP（收信侧） ——

async fn smtp_session(
    mut stream: TcpStream,
    state: Arc<State>,
    tls: SmtpTlsMode,
    acceptor: Option<Arc<tokio_rustls::TlsAcceptor>>,
) {
    if tls == SmtpTlsMode::Implicit {
        // RFC 8314 §3.3：先 TLS 握手，握手完成后服务器才发 220 greeting。
        let acceptor = acceptor.expect("SMTP 隐式 TLS 需 acceptor");
        let Ok(mut tls_stream) = acceptor.accept(stream).await else {
            return;
        };
        if tls_stream
            .write_all(b"220 fake-mail ESMTP\r\n")
            .await
            .is_err()
        {
            return;
        }
        smtp_run(tls_stream, state).await;
        return;
    }
    if stream
        .write_all(b"220 fake-mail ESMTP ready\r\n")
        .await
        .is_err()
    {
        return;
    }
    if tls == SmtpTlsMode::Plain {
        smtp_run(stream, state).await;
        return;
    }

    let acceptor = acceptor.expect("SMTP STARTTLS 需 acceptor");
    let mut saw_ehlo = false;
    loop {
        let Some(line) = read_line_exact(&mut stream).await.ok().flatten() else {
            return;
        };
        let upper = line.trim_end_matches('\r').to_ascii_uppercase();
        if upper.starts_with("EHLO") {
            saw_ehlo = true;
            let _ = stream
                .write_all(b"250-fake-mail\r\n250-STARTTLS\r\n250 OK\r\n")
                .await;
        } else if upper == "STARTTLS" && saw_ehlo {
            if stream
                .write_all(b"220 Ready to start TLS\r\n")
                .await
                .is_err()
            {
                return;
            }
            if let Ok(tls) = acceptor.accept(stream).await {
                // smtp_run 从 saw_ehlo=false 开始，强制客户端在 TLS 后再次 EHLO。
                smtp_run(tls, state).await;
            }
            return;
        } else if upper == "QUIT" {
            let _ = stream.write_all(b"221 bye\r\n").await;
            return;
        } else {
            let _ = stream.write_all(b"530 Must issue STARTTLS first\r\n").await;
        }
    }
}

async fn smtp_run<S>(stream: S, state: Arc<State>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (r, mut w) = tokio::io::split(stream);
    let mut lines = BufReader::new(r).lines();
    let mut saw_ehlo = false;
    let mut authenticated = false;
    let auth_mode = state.smtp_auth;
    loop {
        let Some(line) = lines.next_line().await.ok().flatten() else {
            break;
        };
        let line = line.trim_end_matches('\r').to_string();
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            saw_ehlo = true;
            // 声明 AUTH：受管账户探测要求 EHLO 明示 AUTH 能力。
            let mut reply = b"250-fake-mail\r\n".to_vec();
            reply.extend_from_slice(auth_mode.ehlo_line());
            reply.extend_from_slice(b"250 OK\r\n");
            let _ = w.write_all(&reply).await;
        } else if upper.starts_with("AUTH PLAIN") {
            if !auth_mode.accepts_plain() {
                let _ = w
                    .write_all(b"504 5.5.4 Unrecognized authentication type\r\n")
                    .await;
                continue;
            }
            // 用原始大小写解码 base64：\0user\0pass。
            let ok = match line.split_whitespace().nth(2).map(decode_base64) {
                Some(secret) if secret.starts_with(b"\0") => {
                    let mut parts = secret[1..].splitn(2, |byte| *byte == 0);
                    let user = parts.next().unwrap_or_default();
                    let pass = parts.next().unwrap_or_default();
                    state.smtp_credentials_match(user, pass)
                }
                _ => false,
            };
            state.log_smtp_auth("PLAIN", ok);
            authenticated |= ok;
            let reply: &[u8] = if ok {
                b"235 2.7.0 Accepted\r\n"
            } else {
                b"535 5.7.8 Authentication failed\r\n"
            };
            let _ = w.write_all(reply).await;
        } else if upper == "AUTH LOGIN" {
            if !auth_mode.accepts_login() {
                let _ = w
                    .write_all(b"504 5.5.4 Unrecognized authentication type\r\n")
                    .await;
                continue;
            }
            // 两步 334 挑战：Username: / Password:（base64）。
            if w.write_all(b"334 VXNlcm5hbWU6\r\n").await.is_err() {
                break;
            }
            let Some(user) = lines.next_line().await.ok().flatten() else {
                break;
            };
            if w.write_all(b"334 UGFzc3dvcmQ6\r\n").await.is_err() {
                break;
            }
            let Some(pass) = lines.next_line().await.ok().flatten() else {
                break;
            };
            let user = decode_base64(user.trim_end_matches('\r'));
            let pass = decode_base64(pass.trim_end_matches('\r'));
            let ok = !user.is_empty() && state.smtp_credentials_match(&user, &pass);
            state.log_smtp_auth("LOGIN", ok);
            authenticated |= ok;
            let reply: &[u8] = if ok {
                b"235 2.7.0 Accepted\r\n"
            } else {
                b"535 5.7.8 Authentication failed\r\n"
            };
            let _ = w.write_all(reply).await;
        } else if !saw_ehlo {
            let _ = w.write_all(b"503 Send EHLO first\r\n").await;
        } else if upper.starts_with("MAIL FROM")
            && state.smtp_credentials.is_some()
            && !authenticated
        {
            let _ = w
                .write_all(b"530 5.7.0 Authentication required\r\n")
                .await;
        } else if upper.starts_with("MAIL FROM") || upper.starts_with("RCPT TO") {
            let _ = w.write_all(b"250 OK\r\n").await;
        } else if upper.starts_with("DATA") {
            let _ = w
                .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                .await;
            let mut raw = Vec::new();
            loop {
                let Some(l) = lines.next_line().await.ok().flatten() else {
                    break;
                };
                if l == "." {
                    break;
                }
                let l = l.trim_end_matches('\r');
                // SMTP transparency: DATA 中所有行首点都由发送端加倍，接收端移除一个。
                raw.extend_from_slice(l.strip_prefix('.').unwrap_or(l).as_bytes());
                raw.extend_from_slice(b"\r\n");
            }
            if raw.is_empty() {
                let _ = w.write_all(b"554 no data\r\n").await;
            } else {
                state.deliver(raw);
                let _ = w.write_all(b"250 OK queued\r\n").await;
            }
        } else if upper.starts_with("QUIT") {
            let _ = w.write_all(b"221 bye\r\n").await;
            break;
        } else {
            let _ = w.write_all(b"250 OK\r\n").await;
        }
    }
}

// —— IMAP（读信/IDLE 侧；可选 STARTTLS / 隐式 TLS） ——

const GREETING_PLAIN: &[u8] = b"* OK [CAPABILITY IMAP4rev1 IDLE] fake-mail ready\r\n";
const GREETING_STARTTLS: &[u8] = b"* OK [CAPABILITY IMAP4rev1 STARTTLS IDLE] fake-mail ready\r\n";

async fn imap_session(
    stream: TcpStream,
    state: Arc<State>,
    tls: Option<ImapTlsMode>,
    acceptor: Option<Arc<tokio_rustls::TlsAcceptor>>,
) {
    match tls {
        None => {
            imap_run(stream, state).await;
        }
        Some(ImapTlsMode::Implicit) => {
            let acc = acceptor.expect("implicit 需 acceptor");
            if let Ok(t) = acc.accept(stream).await {
                imap_run(t, state).await;
            }
        }
        Some(ImapTlsMode::StartTls) => {
            let acc = acceptor.expect("starttls 需 acceptor");
            // 明文协商阶段：逐字节读（OK 后紧跟 ClientHello，不能缓冲）
            let mut plain = stream;
            if plain.write_all(GREETING_STARTTLS).await.is_err() {
                return;
            }
            loop {
                let Some(line) = read_line_exact(&mut plain).await.ok().flatten() else {
                    return;
                };
                let parts: Vec<&str> = line.split_whitespace().collect();
                let (Some(tag), Some(cmd)) = (parts.first(), parts.get(1)) else {
                    continue;
                };
                let tag = tag.to_string();
                match cmd.to_ascii_uppercase().as_str() {
                    "CAPABILITY" => {
                        let _ = plain
                            .write_all(b"* CAPABILITY IMAP4rev1 STARTTLS IDLE\r\n")
                            .await;
                        let _ = plain
                            .write_all(format!("{tag} OK CAPABILITY completed\r\n").as_bytes())
                            .await;
                    }
                    "STARTTLS" => {
                        let _ = plain
                            .write_all(format!("{tag} OK Begin TLS negotiation now\r\n").as_bytes())
                            .await;
                        if let Ok(t) = acc.accept(plain).await {
                            imap_run(t, state).await;
                        }
                        return;
                    }
                    "NOOP" => {
                        let _ = plain
                            .write_all(format!("{tag} OK NOOP completed\r\n").as_bytes())
                            .await;
                    }
                    "LOGOUT" => {
                        let _ = plain.write_all(b"* BYE logging out\r\n").await;
                        let _ = plain
                            .write_all(format!("{tag} OK LOGOUT completed\r\n").as_bytes())
                            .await;
                        return;
                    }
                    _ => {
                        let _ = plain
                            .write_all(format!("{tag} OK completed\r\n").as_bytes())
                            .await;
                    }
                }
            }
        }
    }
}

/// 逐字节读一行（`\n` 结尾，剥离 `\r`）。返回 `Ok(None)` = 对端关闭。
/// 仅用于 STARTTLS 协商前的明文阶段：不预读任何多余字节。
async fn read_line_exact<S: AsyncRead + Unpin>(s: &mut S) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match s.read(&mut byte).await {
            Ok(0) => return Ok(None),
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    Ok(Some(
        String::from_utf8_lossy(&line)
            .trim_end_matches('\r')
            .to_string(),
    ))
}

/// 主命令循环：split（读写分离）+ BufReader，IDLE 推送 EXISTS。明文与 TLS 流通用。
async fn imap_run<S: AsyncRead + AsyncWrite + Unpin>(stream: S, state: Arc<State>) {
    let (r, mut w) = tokio::io::split(stream);
    let mut lines = BufReader::new(r).lines();
    if w.write_all(GREETING_PLAIN).await.is_err() {
        return;
    }

    let mut notify_rx = state.notify.subscribe();
    let mut idling = false;
    let mut pending_exists: Option<usize> = None;
    let mut idle_tag = String::new();
    let mut selected = String::from("INBOX");

    loop {
        enum Ev {
            Line(Option<String>),
            Mail(usize),
        }
        let ev = tokio::select! {
            l = lines.next_line() => Ev::Line(l.ok().flatten()),
            m = notify_rx.recv() => match m { Ok(n) => Ev::Mail(n), Err(_) => continue },
        };
        match ev {
            Ev::Mail(n) => {
                if idling {
                    if w.write_all(format!("* {n} EXISTS\r\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                } else {
                    pending_exists = Some(n);
                }
            }
            Ev::Line(None) => break,
            Ev::Line(Some(line)) => {
                let line = line.trim_end_matches('\r').to_string();
                // RFC 2177：终止 IDLE 的 DONE 不带 tag
                if idling && line.trim() == "DONE" {
                    idling = false;
                    if w.write_all(format!("{idle_tag} OK IDLE terminated\r\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                let (Some(tag), Some(cmd)) = (parts.first(), parts.get(1)) else {
                    continue;
                };
                let tag = tag.to_string();
                let cmd = cmd.to_ascii_uppercase();
                let mut out = Vec::new();

                match cmd.as_str() {
                    "CAPABILITY" => {
                        let advertised = state.capabilities.lock().unwrap().clone();
                        out.extend_from_slice(
                            format!("* CAPABILITY {advertised}\r\n").as_bytes(),
                        );
                        out.extend_from_slice(
                            format!("{tag} OK CAPABILITY completed\r\n").as_bytes(),
                        );
                    }
                    "LOGIN" => {
                        // RFC 3501：userid 与 password 可能带引号；这里只做最小解析。
                        let supplied = parts
                            .get(3)
                            .map(|value| value.trim_matches('"').to_string());
                        let accepted = state.imap_password.lock().unwrap().clone();
                        match (accepted, supplied) {
                            (Some(expected), Some(actual)) if actual == expected => {
                                out.extend_from_slice(
                                    format!("{tag} OK LOGIN completed\r\n").as_bytes(),
                                );
                            }
                            (Some(_), _) => {
                                out.extend_from_slice(
                                    format!("{tag} NO [AUTHENTICATIONFAILED]\r\n").as_bytes(),
                                );
                            }
                            (None, _) => {
                                out.extend_from_slice(
                                    format!("{tag} OK LOGIN completed\r\n").as_bytes(),
                                );
                            }
                        }
                    }
                    "NOOP" => {
                        flush_exists(&mut out, &mut pending_exists);
                        out.extend_from_slice(format!("{tag} OK NOOP completed\r\n").as_bytes());
                    }
                    "SELECT" | "EXAMINE" => {
                        selected = parts
                            .get(2)
                            .map(|value| value.trim_matches('"').to_string())
                            .unwrap_or_else(|| "INBOX".to_string());
                        let n = state.mailbox_count(&selected);
                        out.extend_from_slice(b"* FLAGS (\\Seen \\Flagged \\Answered)\r\n");
                        out.extend_from_slice(format!("* {n} EXISTS\r\n").as_bytes());
                        out.extend_from_slice(format!("* {n} RECENT\r\n").as_bytes());
                        out.extend_from_slice(
                            format!(
                                "* OK [UIDVALIDITY {}] UIDs valid\r\n",
                                state.mailbox_uid_validity(&selected)
                            )
                            .as_bytes(),
                        );
                        out.extend_from_slice(
                            format!(
                                "* OK [UIDNEXT {}] predicted\r\n",
                                state.mailbox_next_uid(&selected)
                            )
                            .as_bytes(),
                        );
                        let access = if cmd == "EXAMINE" {
                            "READ-ONLY"
                        } else {
                            "READ-WRITE"
                        };
                        out.extend_from_slice(
                            format!("{tag} OK [{access}] {cmd} completed\r\n").as_bytes(),
                        );
                        pending_exists = None;
                    }
                    "LIST" => {
                        for (name, attributes) in state.mailbox_listing() {
                            out.extend_from_slice(
                                format!("* LIST ({attributes}) \"/\" \"{name}\"\r\n").as_bytes(),
                            );
                        }
                        out.extend_from_slice(
                            format!("{tag} OK LIST completed\r\n").as_bytes(),
                        );
                    }
                    "IDLE" => {
                        out.extend_from_slice(b"+ idling\r\n");
                        idling = true;
                        idle_tag = tag.clone();
                    }
                    // `UID SEARCH [CHARSET UTF-8] TEXT "..."`：整封报文的子串匹配。
                    // 服务器侧检索的验收只关心「客户端发对了命令、把命中取回来了」。
                    "UID"
                        if parts.get(2).map(|s| s.to_ascii_uppercase()).as_deref()
                            == Some("SEARCH") =>
                    {
                        state.command_log.lock().unwrap().push(line.trim().to_string());
                        let query = parts.get(3..).map(|rest| rest.join(" ")).unwrap_or_default();
                        let needle = query
                            .split('"')
                            .nth(1)
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        let hits: Vec<String> = state
                            .mailbox_snapshot(&selected)
                            .into_iter()
                            .filter(|(_, _, _, raw)| {
                                !needle.is_empty()
                                    && String::from_utf8_lossy(raw)
                                        .to_ascii_lowercase()
                                        .contains(&needle)
                            })
                            .map(|(uid, _, _, _)| uid.to_string())
                            .collect();
                        let listed = if hits.is_empty() {
                            "* SEARCH\r\n".to_string()
                        } else {
                            format!("* SEARCH {}\r\n", hits.join(" "))
                        };
                        out.extend_from_slice(listed.as_bytes());
                        out.extend_from_slice(
                            format!("{tag} OK SEARCH completed\r\n").as_bytes(),
                        );
                    }
                    "UID"
                        if parts.get(2).map(|s| s.to_ascii_uppercase()).as_deref()
                            == Some("FETCH") =>
                    {
                        state.imap_fetches.fetch_add(1, Ordering::Relaxed);
                        let range = parts.get(3).copied().unwrap_or("1:*").to_string();
                        let items = parts.get(4..).map(|rest| rest.join(" ")).unwrap_or_default();
                        let upper_items = items.to_ascii_uppercase();
                        // 标记对账只要 FLAGS，不带正文——与增量抓取区分开。
                        let bodyless = !upper_items.contains("BODY");
                        // 服务器检索取回的是头部：`BODY.PEEK[HEADER]`。
                        let header_only = upper_items.contains("[HEADER]");
                        let want_internal_date = upper_items.contains("INTERNALDATE");
                        // `N:*` 之外还支持逗号分隔的显式 UID 集合（检索命中用）。
                        let explicit: Option<Vec<u64>> = if range.contains(':') {
                            None
                        } else {
                            Some(range.split(',').filter_map(|v| v.parse().ok()).collect())
                        };
                        state.fetch_log.lock().unwrap().push((
                            selected.clone(),
                            range.clone(),
                            items.clone(),
                        ));
                        // 只支持 `N:*`——客户端只发这一种。RFC 3501 的语义：N 大于最大 UID
                        // 时仍返回最大 UID 那一条，这里如实模拟，用来钉死客户端的过滤。
                        let start: u64 = range
                            .split(':')
                            .next()
                            .and_then(|value| value.parse().ok())
                            .unwrap_or(1);
                        let all = state.mailbox_snapshot(&selected);
                        let highest = all.iter().map(|(uid, _, _, _)| *uid).max();
                        let mut emitted = 0usize;
                        for (i, (uid, flags, internal_date, raw)) in all.iter().enumerate() {
                            match &explicit {
                                Some(list) if !list.contains(uid) => continue,
                                None if *uid < start => continue,
                                _ => {}
                            }
                            let seq = i + 1;
                            let internal_attr = match (want_internal_date, internal_date.as_deref()) {
                                (true, Some(dt)) => format!(" INTERNALDATE \"{dt}\""),
                                _ => String::new(),
                            };
                            if header_only {
                                let split = raw
                                    .windows(4)
                                    .position(|w| w == b"\r\n\r\n")
                                    .map(|at| at + 4)
                                    .unwrap_or(raw.len());
                                let header = &raw[..split];
                                let len = header.len();
                                out.extend_from_slice(
                                    format!(
                                        "* {seq} FETCH (UID {uid} FLAGS ({flags}){internal_attr} BODY[HEADER] {{{len}}}\r\n"
                                    )
                                    .as_bytes(),
                                );
                                out.extend_from_slice(header);
                                out.extend_from_slice(b")\r\n");
                                emitted += 1;
                                continue;
                            }
                            if bodyless {
                                out.extend_from_slice(
                                    format!("* {seq} FETCH (UID {uid} FLAGS ({flags}){internal_attr})\r\n")
                                        .as_bytes(),
                                );
                                emitted += 1;
                                continue;
                            }
                            let len = raw.len();
                            out.extend_from_slice(
                                format!(
                                    "* {seq} FETCH (UID {uid} FLAGS ({flags}){internal_attr} BODY[] {{{len}}}\r\n"
                                )
                                .as_bytes(),
                            );
                            out.extend_from_slice(raw);
                            out.extend_from_slice(b")\r\n");
                            emitted += 1;
                        }
                        // 对账用的全量请求即使空集也如实返回空——存在性对账依赖这一点。
                        if emitted == 0 && !bodyless && !header_only && explicit.is_none() {
                            if let Some(highest) = highest {
                                let (index, (uid, flags, internal_date, raw)) = all
                                    .iter()
                                    .enumerate()
                                    .find(|(_, (uid, _, _, _))| *uid == highest)
                                    .expect("最大 UID 必在快照内");
                                let seq = index + 1;
                                let len = raw.len();
                                let internal_attr = match (want_internal_date, internal_date.as_deref()) {
                                    (true, Some(dt)) => format!(" INTERNALDATE \"{dt}\""),
                                    _ => String::new(),
                                };
                                out.extend_from_slice(
                                    format!(
                                        "* {seq} FETCH (UID {uid} FLAGS ({flags}){internal_attr} BODY[] {{{len}}}\r\n"
                                    )
                                    .as_bytes(),
                                );
                                out.extend_from_slice(raw);
                                out.extend_from_slice(b")\r\n");
                            }
                        }
                        out.extend_from_slice(format!("{tag} OK FETCH completed\r\n").as_bytes());
                    }
                    "UID"
                        if matches!(
                            parts.get(2).map(|value| value.to_ascii_uppercase()).as_deref(),
                            Some("STORE") | Some("COPY") | Some("MOVE") | Some("EXPUNGE")
                        ) =>
                    {
                        state.command_log.lock().unwrap().push(line.trim().to_string());
                        let sub = parts[2].to_ascii_uppercase();
                        let uid: u64 = parts.get(3).and_then(|v| v.parse().ok()).unwrap_or(0);
                        match sub.as_str() {
                            "STORE" => {
                                let query = parts.get(4..).map(|r| r.join(" ")).unwrap_or_default();
                                let on = query.starts_with('+');
                                let flag = query
                                    .split(['(', ')'])
                                    .nth(1)
                                    .unwrap_or("")
                                    .trim()
                                    .to_string();
                                if !flag.is_empty() {
                                    state.set_flag(&selected, uid, &flag, on);
                                }
                                out.extend_from_slice(
                                    format!("{tag} OK STORE completed\r\n").as_bytes(),
                                );
                            }
                            "COPY" | "MOVE" => {
                                let target = parts
                                    .get(4)
                                    .map(|value| value.trim_matches('"').to_string())
                                    .unwrap_or_default();
                                // RFC 3501：目标不存在时是 NO [TRYCREATE]，不是隐式创建。
                                let known = State::is_inbox(&target)
                                    || state.extra.lock().unwrap().contains_key(&target);
                                if !known {
                                    out.extend_from_slice(
                                        format!("{tag} NO [TRYCREATE] no such mailbox\r\n")
                                            .as_bytes(),
                                    );
                                    if w.write_all(&out).await.is_err() {
                                        break;
                                    }
                                    continue;
                                }
                                match state.take_raw(&selected, uid) {
                                    Some((flags, internal_date, raw)) => {
                                        state.append(&target, &flags, internal_date, raw);
                                        if sub == "MOVE" {
                                            state.remove(&selected, uid);
                                        }
                                        out.extend_from_slice(
                                            format!("{tag} OK {sub} completed\r\n").as_bytes(),
                                        );
                                    }
                                    None => out.extend_from_slice(
                                        format!("{tag} NO no such message\r\n").as_bytes(),
                                    ),
                                }
                            }
                            _ => {
                                // UID EXPUNGE：只清掉指定 uid 且带 \\Deleted 的那条。
                                let deleted = state
                                    .mailbox_snapshot(&selected)
                                    .into_iter()
                                    .any(|(existing, flags, _, _)| {
                                        existing == uid
                                            && flags.to_ascii_lowercase().contains("deleted")
                                    });
                                if deleted {
                                    state.remove(&selected, uid);
                                }
                                out.extend_from_slice(
                                    format!("{tag} OK EXPUNGE completed\r\n").as_bytes(),
                                );
                            }
                        }
                    }
                    "APPEND" => {
                        // RFC 3501：`APPEND <mailbox> [flags] {N}` → `+` 续行 → N 字节字面量。
                        state.command_log.lock().unwrap().push(line.trim().to_string());
                        let target = parts
                            .get(2)
                            .map(|value| value.trim_matches('"').to_string())
                            .unwrap_or_default();
                        let literal: usize = parts
                            .last()
                            .and_then(|value| {
                                value.trim_matches(|c| c == '{' || c == '}' || c == '+').parse().ok()
                            })
                            .unwrap_or(0);
                        if w.write_all(b"+ Ready for literal data\r\n").await.is_err() {
                            break;
                        }
                        // 逐行读到字面量长度为止（测试内容一律 CRLF 文本）。
                        let mut raw: Vec<u8> = Vec::with_capacity(literal);
                        while raw.len() < literal {
                            let Some(chunk) = lines.next_line().await.ok().flatten() else {
                                break;
                            };
                            raw.extend_from_slice(chunk.as_bytes());
                            raw.extend_from_slice(b"\r\n");
                        }
                        raw.truncate(literal);
                        state.append(&target, "", Some(now_imap_internal_date()), raw);
                        out.extend_from_slice(
                            format!("{tag} OK APPEND completed\r\n").as_bytes(),
                        );
                    }
                    "LOGOUT" => {
                        out.extend_from_slice(b"* BYE logging out\r\n");
                        out.extend_from_slice(format!("{tag} OK LOGOUT completed\r\n").as_bytes());
                        let _ = w.write_all(&out).await;
                        break;
                    }
                    _ => {
                        out.extend_from_slice(format!("{tag} OK completed\r\n").as_bytes());
                    }
                }
                if w.write_all(&out).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// 最小 base64 解码（标准字母表 + 填充），AUTH PLAIN 测试用。
fn decode_base64(value: &str) -> Vec<u8> {
    const T: &[u8; 128] = &{
        let mut table = [255u8; 128];
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < alphabet.len() {
            table[alphabet[i] as usize] = i as u8;
            i += 1;
        }
        table
    };
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in value.bytes() {
        if byte == b'=' {
            break;
        }
        let value = T[byte as usize & 127];
        if value == 255 {
            return out;
        }
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    out
}

fn flush_exists(out: &mut Vec<u8>, pending: &mut Option<usize>) {
    if let Some(n) = pending.take() {
        out.extend_from_slice(format!("* {n} EXISTS\r\n").as_bytes());
    }
}
