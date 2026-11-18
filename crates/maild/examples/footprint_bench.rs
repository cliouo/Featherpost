//! Release 守护进程低占用后台收信实测基准（ADR-0041 验收取证）。
//!
//! 完全自包含、本地回环 TLS：
//!
//! 1. 起 3 个本地自签 TLS 假邮件服务器（IMAP 993 风格隐式 TLS + SMTP 465 风格隐式 TLS，
//!    SMTP 只宣告 `AUTH LOGIN` 且校验凭据）。主信箱预置 1,500 封历史邮件（中文纯文本 /
//!    中文 HTML / 内联图片 / 普通附件四种形态轮换），另两个账户各 300 封；
//! 2. 以**独立子进程**拉起同目录下 release 编译的守护进程（`maild_brand::CLI_DAEMON`）（无 GUI / WebView），
//!    装配 store + receive(idle, initial_sync_limit=200) + filters + rules(persist,
//!    notify_new_mail) + otp + notify(dev-log) + scheduler + transport(SMTPS 465) + api(unix)；
//! 3. 经 Unix socket JSON-RPC 测量：冷启动耗时、稳态 RSS / `phys_footprint` 与 5 秒空闲 CPU、
//!    30 封实时投递的 IDLE 推送 P50/P95 延迟与通知限流、注入后内存与 WAL 复测、
//!    服务器检索 + 按需拉回冷启动窗口之外的历史邮件。
//!
//! 运行：
//!
//! ```sh
//! cargo build --release -p maild --examples --bins --locked
//! target/release/examples/footprint_bench            # 表格 + JSON
//! target/release/examples/footprint_bench --json     # 只输出 JSON
//! ```
//!
//! 任何一项验收不成立时以非零码退出。只支持 Unix（macOS 上额外报告 `phys_footprint`）。

#[cfg(unix)]
#[path = "../tests/common/fake_mail.rs"]
#[allow(dead_code)]
mod fake_mail;

#[cfg(unix)]
fn main() {
    let json_only = std::env::args().any(|arg| arg == "--json");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match runtime.block_on(bench::run(json_only)) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("footprint_bench 失败: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("footprint_bench 只支持 Unix（需要 unix socket api 传输与 ps 采样）");
}

#[cfg(unix)]
mod bench {
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use base64::Engine as _;
    use serde_json::{json, Value};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;
    use tokio::process::{Child, Command};
    use tokio::sync::{mpsc, oneshot};

    use super::fake_mail::{FakeMail, SmtpAuthMode};
    use maild::transport::{Transport, TransportConfig};

    const ACCOUNTS: [(&str, usize); 3] = [("main", 1_500), ("work", 300), ("alerts", 300)];
    const INITIAL_SYNC_LIMIT: usize = 200;
    const IDLE_SAMPLE_SECS: u64 = 5;
    const OTP_PER_ACCOUNT: usize = 2;
    const NORMAL_PER_ACCOUNT: usize = 8;
    const HISTORY_TARGET_UID: usize = 5;
    const PASSWORD: &str = "bench-secret";

    type BenchResult<T> = Result<T, String>;

    fn user_of(account: &str) -> String {
        format!("{account}@bench.local")
    }

    /// 历史邮件里可被服务器 `SEARCH TEXT` 唯一命中的令牌（不含 6 位连续数字，免得被当成验证码）。
    fn history_token(account: &str, index: usize) -> String {
        format!("{}ARCHIVE{index:04}Q", account.to_ascii_uppercase())
    }

    fn history_text(account: &str, index: usize) -> String {
        format!(
            "这是 {account} 信箱的第 {index} 封历史邮件。\r\n\
             季度回顾：项目进展顺利，附件中是会议纪要与示意图。\r\n\
             检索令牌 {}。\r\n",
            history_token(account, index)
        )
    }

    fn b64_wrapped(bytes: &[u8]) -> String {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut out = String::with_capacity(encoded.len() + encoded.len() / 76 * 2 + 2);
        for chunk in encoded.as_bytes().chunks(76) {
            out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
            out.push_str("\r\n");
        }
        out
    }

    fn encoded_word(text: &str) -> String {
        format!(
            "=?UTF-8?B?{}?=",
            base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
        )
    }

    /// 2025 年的固定日期：早于通知的 24 小时积压阈值，冷启动不应弹任何通知。
    fn history_date(index: usize) -> String {
        let day = 1 + index % 28;
        let hour = index % 24;
        let minute = index % 60;
        format!("Mon, {day} Sep 2025 {hour:02}:{minute:02}:00 +0800")
    }

    /// 四种形态轮换：纯文本 / HTML 备选 / HTML + 内联图片 / 纯文本 + 普通附件。
    fn history_mail(account: &str, index: usize) -> Vec<u8> {
        let text = history_text(account, index);
        let token = history_token(account, index);
        let subject = format!("{token} {}", encoded_word(&format!("季度回顾 第{index}封")));
        let head = format!(
            "From: {} <reports@bench.local>\r\nTo: {}\r\nSubject: {subject}\r\n\
             Date: {}\r\nMessage-ID: <hist-{account}-{index}@bench.local>\r\nMIME-Version: 1.0\r\n",
            encoded_word("周报机器人"),
            user_of(account),
            history_date(index),
        );
        let html = format!(
            "<html><body><h1>季度回顾</h1><p>{}</p><p><img src=\"cid:chart{index}@bench\"></p></body></html>",
            text.replace("\r\n", "<br>")
        );
        let attachment: Vec<u8> = (0..2048u32)
            .map(|value| (value.wrapping_mul(31).wrapping_add(index as u32) % 251) as u8)
            .collect();
        let body = match index % 4 {
            0 => format!(
                "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}",
                b64_wrapped(text.as_bytes())
            ),
            1 => format!(
                "Content-Type: multipart/alternative; boundary=\"alt{index}\"\r\n\r\n\
                 --alt{index}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\
                 --alt{index}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\
                 --alt{index}--\r\n",
                b64_wrapped(text.as_bytes()),
                b64_wrapped(html.as_bytes()),
            ),
            2 => format!(
                "Content-Type: multipart/related; boundary=\"rel{index}\"; type=\"multipart/alternative\"\r\n\r\n\
                 --rel{index}\r\nContent-Type: multipart/alternative; boundary=\"alt{index}\"\r\n\r\n\
                 --alt{index}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\
                 --alt{index}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\
                 --alt{index}--\r\n\
                 --rel{index}\r\nContent-Type: image/png\r\nContent-Transfer-Encoding: base64\r\n\
                 Content-ID: <chart{index}@bench>\r\nContent-Disposition: inline; filename=\"chart.png\"\r\n\r\n{}\
                 --rel{index}--\r\n",
                b64_wrapped(text.as_bytes()),
                b64_wrapped(html.as_bytes()),
                b64_wrapped(&attachment[..1024]),
            ),
            _ => format!(
                "Content-Type: multipart/mixed; boundary=\"mix{index}\"\r\n\r\n\
                 --mix{index}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\
                 --mix{index}\r\nContent-Type: application/octet-stream; name=\"minutes-{index}.bin\"\r\n\
                 Content-Transfer-Encoding: base64\r\nContent-Disposition: attachment; filename=\"minutes-{index}.bin\"\r\n\r\n{}\
                 --mix{index}--\r\n",
                b64_wrapped(text.as_bytes()),
                b64_wrapped(&attachment),
            ),
        };
        format!("{head}{body}").into_bytes()
    }

    // —— JSON-RPC over unix socket ——

    struct Push {
        account: String,
        subject: String,
        at: Instant,
    }

    struct Rpc {
        writer: tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>,
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
        next_id: AtomicU64,
    }

    impl Rpc {
        async fn connect(socket: &Path) -> BenchResult<(Rpc, mpsc::UnboundedReceiver<Push>)> {
            let stream = UnixStream::connect(socket)
                .await
                .map_err(|error| format!("连接 {}: {error}", socket.display()))?;
            let (read, write) = stream.into_split();
            let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> = Arc::default();
            let (push_tx, push_rx) = mpsc::unbounded_channel();
            let routes = Arc::clone(&pending);
            tokio::spawn(async move {
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let at = Instant::now();
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if value.get("method").and_then(Value::as_str) == Some("message/received") {
                        let params = &value["params"];
                        let _ = push_tx.send(Push {
                            account: params["account"].as_str().unwrap_or_default().to_string(),
                            subject: params["subject"].as_str().unwrap_or_default().to_string(),
                            at,
                        });
                        continue;
                    }
                    if let Some(id) = value.get("id").and_then(Value::as_u64) {
                        if let Some(waiter) = routes.lock().unwrap().remove(&id) {
                            let _ = waiter.send(value);
                        }
                    }
                }
            });
            Ok((
                Rpc {
                    writer: tokio::sync::Mutex::new(write),
                    pending,
                    next_id: AtomicU64::new(1),
                },
                push_rx,
            ))
        }

        async fn call(&self, method: &str, params: Value) -> BenchResult<Value> {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = oneshot::channel();
            self.pending.lock().unwrap().insert(id, tx);
            let mut frame = serde_json::to_vec(
                &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
            )
            .map_err(|error| error.to_string())?;
            frame.push(b'\n');
            self.writer
                .lock()
                .await
                .write_all(&frame)
                .await
                .map_err(|error| format!("{method} 写入失败: {error}"))?;
            let reply = tokio::time::timeout(Duration::from_secs(60), rx)
                .await
                .map_err(|_| format!("{method} 60s 内无应答"))?
                .map_err(|_| format!("{method} 连接已断开"))?;
            if let Some(error) = reply.get("error") {
                return Err(format!("{method} 失败: {error}"));
            }
            Ok(reply["result"].clone())
        }
    }

    // —— 进程采样 ——

    struct ProcSample {
        rss_kib: u64,
        cpu_time: Duration,
    }

    /// `ps -o rss=,time=`：常驻 KiB + 累计 CPU 时间（`[[dd-]hh:]mm:ss.cc`）。
    async fn ps_sample(pid: u32) -> BenchResult<ProcSample> {
        let output = Command::new("ps")
            .args(["-o", "rss=,time=", "-p", &pid.to_string()])
            .output()
            .await
            .map_err(|error| format!("ps: {error}"))?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut fields = text.split_whitespace();
        let rss_kib = fields
            .next()
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| format!("ps 输出无法解析: {text:?}"))?;
        let time = fields
            .next()
            .ok_or_else(|| format!("ps 输出无法解析: {text:?}"))?;
        Ok(ProcSample {
            rss_kib,
            cpu_time: parse_cpu_time(time)?,
        })
    }

    fn parse_cpu_time(value: &str) -> BenchResult<Duration> {
        let (days, rest) = match value.split_once('-') {
            Some((days, rest)) => (days.parse::<u64>().map_err(|e| e.to_string())?, rest),
            None => (0, value),
        };
        let mut seconds = 0f64;
        for part in rest.split(':') {
            seconds = seconds * 60.0 + part.parse::<f64>().map_err(|e| format!("{value}: {e}"))?;
        }
        Ok(Duration::from_secs_f64(days as f64 * 86_400.0 + seconds))
    }

    /// 子进程累计 CPU 时间（纳秒）与唤醒次数：macOS 走 `proc_pid_rusage`（ns 精度，
    /// `ps` 的 `time` 只有 10 ms 分辨率，空闲窗口里读出来恒为 0）。其他平台返回 None。
    #[cfg(target_os = "macos")]
    fn rusage_sample(pid: u32) -> Option<(u64, u64)> {
        #[repr(C)]
        struct Timebase {
            numer: u32,
            denom: u32,
        }
        extern "C" {
            fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
        }
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: 缓冲是一整个 rusage_info_v4，flavor 与之匹配；内核只写不读。
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid as libc::c_int,
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr() as *mut libc::rusage_info_t,
            )
        };
        if rc != 0 {
            return None;
        }
        // SAFETY: rc == 0 表示内核已填充整个结构。
        let info = unsafe { info.assume_init() };
        let mut timebase = Timebase { numer: 0, denom: 0 };
        // SAFETY: 只写入调用方提供的结构体。
        if unsafe { mach_timebase_info(&mut timebase) } != 0 || timebase.denom == 0 {
            return None;
        }
        let ticks = info.ri_user_time.saturating_add(info.ri_system_time) as u128;
        let nanos = ticks * timebase.numer as u128 / timebase.denom as u128;
        let wakeups = info.ri_pkg_idle_wkups.saturating_add(info.ri_interrupt_wkups);
        Some((nanos as u64, wakeups))
    }

    #[cfg(not(target_os = "macos"))]
    fn rusage_sample(_pid: u32) -> Option<(u64, u64)> {
        None
    }

    async fn ps_pcpu(pid: u32) -> f64 {
        Command::new("ps")
            .args(["-o", "pcpu=", "-p", &pid.to_string()])
            .output()
            .await
            .ok()
            .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().parse().ok())
            .unwrap_or(f64::NAN)
    }

    #[derive(Clone, Copy, Default)]
    struct Runtime {
        rss: u64,
        footprint: u64,
        db: u64,
        wal: u64,
        stored: u64,
    }

    fn runtime_of(status: &Value) -> Runtime {
        let field = |key: &str| status["runtime"][key].as_u64().unwrap_or(0);
        Runtime {
            rss: field("rss_bytes"),
            footprint: field("phys_footprint_bytes"),
            db: field("store_db_bytes"),
            wal: field("store_wal_bytes"),
            stored: field("stored_messages"),
        }
    }

    fn mib(bytes: u64) -> f64 {
        bytes as f64 / (1024.0 * 1024.0)
    }

    fn percentile(sorted: &[f64], p: f64) -> f64 {
        if sorted.is_empty() {
            return f64::NAN;
        }
        let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
        sorted[rank.clamp(1, sorted.len()) - 1]
    }

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or(0)
    }

    fn check(ok: bool, what: String, failures: &mut Vec<String>) {
        if !ok {
            failures.push(what);
        }
    }

    // —— 守护进程 ——

    struct Daemon {
        child: Child,
        notifications: Arc<AtomicU64>,
        stderr_tail: Arc<Mutex<Vec<String>>>,
    }

    impl Daemon {
        fn pid(&self) -> u32 {
            self.child.id().unwrap_or(0)
        }

        fn tail(&self) -> String {
            self.stderr_tail.lock().unwrap().join("\n")
        }

        async fn terminate(mut self) -> BenchResult<Option<i32>> {
            let pid = self.pid() as libc::pid_t;
            // SAFETY: 向自己拉起、尚未回收的子进程发 SIGTERM。
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
            match tokio::time::timeout(Duration::from_secs(15), self.child.wait()).await {
                Ok(Ok(status)) => Ok(status.code()),
                _ => {
                    let _ = self.child.kill().await;
                    Err("守护进程 15s 内未响应 SIGTERM 退出".into())
                }
            }
        }
    }

    fn daemon_binary() -> BenchResult<PathBuf> {
        if let Some(explicit) = std::env::var_os("MAILD_BENCH_DAEMON") {
            return Ok(PathBuf::from(explicit));
        }
        let me = std::env::current_exe().map_err(|error| error.to_string())?;
        // target/<profile>/examples/footprint_bench → target/<profile>/<CLI_DAEMON>
        let candidate = me
            .parent()
            .and_then(Path::parent)
            .map(|dir| dir.join(maild_brand::CLI_DAEMON))
            .ok_or("无法定位守护进程二进制")?;
        if !candidate.is_file() {
            return Err(format!(
                "{} 不存在：先 cargo build --release -p maild --examples --bins",
                candidate.display()
            ));
        }
        Ok(candidate)
    }

    fn write_config(root: &Path, servers: &[(&str, &FakeMail)]) -> BenchResult<(PathBuf, PathBuf)> {
        let database = root.join("data").join(maild_brand::DATABASE_FILE_NAME);
        let socket = root.join("run").join(maild_brand::SOCKET_FILE_NAME);
        let (_, main_server) = servers[0];
        let mut text = format!(
            r#"
[plugins.filters]
[plugins.filters.config]
[[plugins.filters.config.rules]]
field = "subject"
pattern = "广告"
action = "drop"

[plugins.store]
[plugins.store.config]
db_path = "{db}"

[plugins.rules]
[plugins.rules.config]
show_sensitive = true
notify_new_mail = true
persist = true

[plugins.notify]
[plugins.notify.config]
provider = "dev-log"

[plugins.otp]
[plugins.otp.config]

[plugins.transport]
[plugins.transport.config]
server = "127.0.0.1"
port = {smtp}
tls = "implicit"
accept_invalid_certs = true
username = "{user}"
password = "{PASSWORD}"

[plugins.api]
[plugins.api.config]
transport = "unix"
listen = "{sock}"

[plugins.scheduler]
[plugins.scheduler.config]
jitter_secs = 1
"#,
            db = database.display(),
            sock = socket.display(),
            smtp = main_server.smtp_addr.port(),
            user = user_of("main"),
        );
        for (account, server) in servers {
            text.push_str(&format!(
                r#"
[[accounts]]
account = "{account}"
server = "127.0.0.1"
port = {port}
username = "{user}"
password = "{PASSWORD}"
tls = "implicit"
accept_invalid_certs = true
idle = true
initial_sync_limit = {INITIAL_SYNC_LIMIT}
"#,
                port = server.imap_addr.port(),
                user = user_of(account),
            ));
        }
        let config = root.join(maild_brand::CONFIG_FILE_NAME);
        std::fs::write(&config, text).map_err(|error| error.to_string())?;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
        Ok((config, socket))
    }

    async fn spawn_daemon(binary: &Path, root: &Path, config: &Path) -> BenchResult<Daemon> {
        let mut command = Command::new(binary);
        command
            .arg("--config")
            .arg(config)
            .current_dir(root)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("xdg-config"))
            .env("XDG_DATA_HOME", root.join("xdg-data"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("XDG_STATE_HOME", root.join("xdg-state"))
            .env_remove(maild_brand::CONFIG_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| format!("拉起 {}: {error}", binary.display()))?;
        let notifications = Arc::new(AtomicU64::new(0));
        let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::default();
        let stderr = child.stderr.take().ok_or("stderr 未接管")?;
        {
            let notifications = Arc::clone(&notifications);
            let tail = Arc::clone(&stderr_tail);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.starts_with("[maild/notify] notification ") && line.ends_with(" submitted")
                    {
                        notifications.fetch_add(1, Ordering::Relaxed);
                    }
                    let mut tail = tail.lock().unwrap();
                    if tail.len() == 40 {
                        tail.remove(0);
                    }
                    tail.push(line);
                }
            });
        }
        Ok(Daemon {
            child,
            notifications,
            stderr_tail,
        })
    }

    async fn wait_for_socket(socket: &Path, daemon: &mut Daemon) -> BenchResult<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if socket.exists() {
                return Ok(());
            }
            if let Ok(Some(status)) = daemon.child.try_wait() {
                return Err(format!("守护进程提前退出 {status}:\n{}", daemon.tail()));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Err(format!("20s 内未创建 socket:\n{}", daemon.tail()))
    }

    /// 等待一组主题各自的 `message/received` 推送，返回 主题 → 到达时刻。
    async fn collect_pushes(
        pushes: &mut mpsc::UnboundedReceiver<Push>,
        wanted: &HashMap<String, String>,
        timeout: Duration,
    ) -> HashMap<String, Instant> {
        let mut arrived = HashMap::new();
        let deadline = tokio::time::Instant::now() + timeout;
        while arrived.len() < wanted.len() {
            match tokio::time::timeout_at(deadline, pushes.recv()).await {
                Ok(Some(push)) => {
                    if wanted.get(&push.subject) == Some(&push.account) {
                        arrived.entry(push.subject).or_insert(push.at);
                    }
                }
                _ => break,
            }
        }
        arrived
    }

    fn smtp_for(server: &FakeMail, account: &str) -> Transport {
        Transport::from_config(TransportConfig {
            server: "127.0.0.1".into(),
            port: server.smtp_addr.port(),
            username: user_of(account),
            password: PASSWORD.into(),
            tls: "implicit".into(),
            accept_invalid_certs: true,
        })
    }

    pub async fn run(json_only: bool) -> BenchResult<()> {
        let binary = daemon_binary()?;
        // macOS 的 /tmp 是指向 /private/tmp 的符号链接，而 api/store 拒绝符号链接祖先目录：
        // 直接在规范路径下建根目录。
        let base = if Path::new("/private/tmp").is_dir() {
            PathBuf::from("/private/tmp")
        } else {
            std::env::temp_dir()
                .canonicalize()
                .map_err(|error| error.to_string())?
        };
        let scratch = tempfile::Builder::new()
            .prefix("mfb")
            .tempdir_in(&base)
            .map_err(|error| error.to_string())?;
        let root = scratch.path().to_path_buf();
        for dir in ["data", "run", "home", "xdg-config", "xdg-data", "xdg-state"] {
            std::fs::create_dir_all(root.join(dir)).map_err(|error| error.to_string())?;
        }
        for dir in [root.clone(), root.join("run"), root.join("data")] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| error.to_string())?;
        }

        // 1. 假服务器 + 历史邮件。
        let seed_started = Instant::now();
        let mut servers = Vec::new();
        for (account, history) in ACCOUNTS {
            let server =
                FakeMail::start_all_implicit_tls(SmtpAuthMode::LoginOnly, &user_of(account), PASSWORD)
                    .await
                    .map_err(|error| format!("假服务器 {account}: {error}"))?;
            for index in 1..=history {
                server.deliver_to("INBOX", "", history_mail(account, index));
            }
            servers.push((account, server));
        }
        let seed_ms = seed_started.elapsed().as_millis();
        let server_refs: Vec<(&str, &FakeMail)> =
            servers.iter().map(|(name, server)| (*name, server)).collect();
        let (config, socket) = write_config(&root, &server_refs)?;
        let mut failures = Vec::new();

        // 2. 冷启动：进程拉起 → 3 账户 online + 各自最新 200 封落库 + FTS 索引收敛。
        let spawned = Instant::now();
        let mut daemon = spawn_daemon(&binary, &root, &config).await?;
        wait_for_socket(&socket, &mut daemon).await?;
        let socket_ms = spawned.elapsed().as_millis();
        let (rpc, mut pushes) = Rpc::connect(&socket).await?;
        let expected_window = (ACCOUNTS.len() * INITIAL_SYNC_LIMIT) as u64;
        let mut online_ms = None;
        let cold_ms = loop {
            let status = rpc.call("status", json!({})).await?;
            let accounts = status["accounts"].as_array().cloned().unwrap_or_default();
            let all_online = accounts.len() == ACCOUNTS.len()
                && accounts.iter().all(|entry| entry["state"] == "online");
            if all_online && online_ms.is_none() {
                online_ms = Some(spawned.elapsed().as_millis());
            }
            if all_online && runtime_of(&status).stored >= expected_window {
                let index = rpc.call("getSearchIndexStatus", json!({})).await?;
                if index["pending"].as_u64() == Some(0) {
                    break spawned.elapsed().as_millis();
                }
            }
            if spawned.elapsed() > Duration::from_secs(180) {
                return Err(format!("冷启动 180s 未收敛: {status}\n{}", daemon.tail()));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let cold_status = runtime_of(&rpc.call("status", json!({})).await?);
        check(
            cold_status.stored == expected_window,
            format!(
                "冷启动应只落库 {expected_window} 封（3×{INITIAL_SYNC_LIMIT}），实际 {}",
                cold_status.stored
            ),
            &mut failures,
        );
        let mut lowest_body_uid: HashMap<String, u64> = HashMap::new();
        for (account, server) in &servers {
            let history = ACCOUNTS
                .iter()
                .find(|(name, _)| name == account)
                .map(|(_, count)| *count)
                .unwrap_or(0) as u64;
            for (_, range, items) in server.fetch_log() {
                if !items.to_ascii_uppercase().contains("BODY") {
                    continue;
                }
                let low = range
                    .split([':', ','])
                    .filter_map(|value| value.parse::<u64>().ok())
                    .min()
                    .unwrap_or(0);
                let window_start = history + 1 - INITIAL_SYNC_LIMIT as u64;
                check(
                    low >= window_start,
                    format!("{account} 冷启动抓取了窗口外正文: {range}"),
                    &mut failures,
                );
                let entry = lowest_body_uid.entry(account.to_string()).or_insert(u64::MAX);
                *entry = (*entry).min(low);
            }
        }
        let cold_notifications = daemon.notifications.load(Ordering::Relaxed);
        check(
            cold_notifications == 0,
            format!("历史积压信不应弹通知，实际 {cold_notifications}"),
            &mut failures,
        );
        // 冷启动推送不计入后续延迟统计。
        while pushes.try_recv().is_ok() {}

        // 3. 稳态：静置 3s，再采样 5s 空闲窗口。
        tokio::time::sleep(Duration::from_secs(3)).await;
        while pushes.try_recv().is_ok() {}
        let pid = daemon.pid();
        let idle_before = runtime_of(&rpc.call("status", json!({})).await?);
        let ps_start = ps_sample(pid).await?;
        let rusage_start = rusage_sample(pid);
        let wall_start = Instant::now();
        let mut ps_rss = Vec::new();
        let mut ps_pcpu_samples = Vec::new();
        for _ in 0..IDLE_SAMPLE_SECS {
            tokio::time::sleep(Duration::from_secs(1)).await;
            ps_rss.push(ps_sample(pid).await?.rss_kib);
            ps_pcpu_samples.push(ps_pcpu(pid).await);
        }
        let rusage_end = rusage_sample(pid);
        let idle_wall = wall_start.elapsed();
        let ps_end = ps_sample(pid).await?;
        let idle_after = runtime_of(&rpc.call("status", json!({})).await?);
        let (idle_cpu_ms, idle_wakeups, cpu_source) = match (rusage_start, rusage_end) {
            (Some((cpu0, wake0)), Some((cpu1, wake1))) => (
                cpu1.saturating_sub(cpu0) as f64 / 1_000_000.0,
                Some(wake1.saturating_sub(wake0)),
                "proc_pid_rusage",
            ),
            _ => (
                ps_end.cpu_time.saturating_sub(ps_start.cpu_time).as_secs_f64() * 1000.0,
                None,
                "ps time",
            ),
        };
        let idle_cpu_pct = idle_cpu_ms / (idle_wall.as_secs_f64() * 1000.0) * 100.0;
        let idle_ps_rss_kib = ps_rss.iter().copied().max().unwrap_or(ps_end.rss_kib);
        let idle_ps_pcpu_max = ps_pcpu_samples.iter().copied().fold(0f64, f64::max);

        // 4. 实时 IDLE 推送：每账户 2 封验证码信 + 8 封普通信 = 30 封，经 SMTPS 465
        //    (`AUTH LOGIN`) 投递到各自服务器，IMAP IDLE → message/received。
        let mut latencies = Vec::new();
        let mut wanted_all = 0usize;
        let mut arrived_all = 0usize;
        let base_notifications = daemon.notifications.load(Ordering::Relaxed);
        let mut phase_notifications = Vec::new();
        for (phase, per_account, gap_ms) in [
            ("otp", OTP_PER_ACCOUNT, 400u64),
            ("normal", NORMAL_PER_ACCOUNT, 120u64),
        ] {
            let before = daemon.notifications.load(Ordering::Relaxed);
            let mut sent_at: HashMap<String, Instant> = HashMap::new();
            let mut wanted: HashMap<String, String> = HashMap::new();
            for round in 0..per_account {
                for (account, server) in &servers {
                    let seq = sent_at.len() + 1;
                    let (subject, body) = if phase == "otp" {
                        (
                            format!("登录验证码 LIVE-{phase}-{account}-{round}"),
                            format!(
                                "您的验证码是 {}，5 分钟内有效，请勿泄露。",
                                483_920 + seq * 7
                            ),
                        )
                    } else {
                        (
                            format!("项目周报 LIVE-{phase}-{account}-{round}"),
                            "本周进展：后台收信内核压测进行中，详见附件说明。".to_string(),
                        )
                    };
                    let transport = smtp_for(server, account);
                    let started = Instant::now();
                    transport
                        .send("sender@bench.local", &user_of(account), &subject, &body)
                        .await
                        .map_err(|error| format!("SMTPS 投递 {subject}: {error}"))?;
                    sent_at.insert(subject.clone(), started);
                    wanted.insert(subject, account.to_string());
                    tokio::time::sleep(Duration::from_millis(gap_ms)).await;
                }
            }
            let arrived = collect_pushes(&mut pushes, &wanted, Duration::from_secs(30)).await;
            wanted_all += wanted.len();
            arrived_all += arrived.len();
            for (subject, at) in &arrived {
                if let Some(started) = sent_at.get(subject) {
                    latencies.push(at.saturating_duration_since(*started).as_secs_f64() * 1000.0);
                }
            }
            // 通知 worker 是异步的：给它一点时间把 dev-log 行写出来。
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let after = daemon.notifications.load(Ordering::Relaxed);
            phase_notifications.push((phase, wanted.len(), after - before));
        }
        check(
            arrived_all == wanted_all,
            format!("实时推送 {arrived_all}/{wanted_all} 封到达"),
            &mut failures,
        );
        latencies.sort_by(|a, b| a.total_cmp(b));
        let otp_notified = phase_notifications[0].2;
        let normal_notified = phase_notifications[1].2;
        let expected_otp = (ACCOUNTS.len() * OTP_PER_ACCOUNT) as u64;
        // 同账户 10 秒窗口超过 3 封：前 3 封逐封 + 1 条合并提示，其余抑制。
        let expected_normal = (ACCOUNTS.len() * 4) as u64;
        check(
            otp_notified == expected_otp,
            format!("验证码通知应恰好 {expected_otp} 条，实际 {otp_notified}"),
            &mut failures,
        );
        check(
            normal_notified == expected_normal,
            format!("普通新邮件限流后应 {expected_normal} 条，实际 {normal_notified}"),
            &mut failures,
        );
        let live_notifications = daemon.notifications.load(Ordering::Relaxed) - base_notifications;

        // 5. 注入后复测。
        tokio::time::sleep(Duration::from_secs(3)).await;
        let after_live = runtime_of(&rpc.call("status", json!({})).await?);
        let after_live_ps = ps_sample(pid).await?;
        check(
            after_live.stored == expected_window + wanted_all as u64,
            format!(
                "注入后应落库 {}，实际 {}",
                expected_window + wanted_all as u64,
                after_live.stored
            ),
            &mut failures,
        );
        let footprint_growth = after_live.footprint as i64 - idle_after.footprint as i64;
        check(
            footprint_growth < 16 * 1024 * 1024,
            format!("注入 30 封后 phys_footprint 膨胀 {footprint_growth} 字节"),
            &mut failures,
        );
        check(
            after_live.wal < 8 * 1024 * 1024,
            format!("WAL 异常膨胀 {} 字节", after_live.wal),
            &mut failures,
        );

        // 6. 守护进程侧 SMTPS 465 发信：`send` RPC → 全局 transport（隐式 TLS + AUTH LOGIN）。
        let daemon_send_started = Instant::now();
        let daemon_subject = "守护进程发信回环 LIVE-daemon-send".to_string();
        rpc.call(
            "send",
            json!({
                "from": user_of("main"),
                "to": [user_of("main")],
                "subject": daemon_subject,
                "body": "经守护进程 SMTPS 465 发出，再经 IDLE 收回。",
            }),
        )
        .await?;
        let wanted_send: HashMap<String, String> =
            [(daemon_subject.clone(), "main".to_string())].into();
        let looped = collect_pushes(&mut pushes, &wanted_send, Duration::from_secs(15)).await;
        let daemon_send_roundtrip_ms = looped
            .get(&daemon_subject)
            .map(|at| at.saturating_duration_since(daemon_send_started).as_millis());
        check(
            daemon_send_roundtrip_ms.is_some(),
            "守护进程 send（SMTPS 465）回环邮件未经 IDLE 收回".into(),
            &mut failures,
        );
        let main_auth = servers[0].1.smtp_auth_log();
        check(
            !main_auth.is_empty()
                && main_auth
                    .iter()
                    .all(|(mechanism, ok)| *ok && mechanism.eq_ignore_ascii_case("LOGIN")),
            format!("SMTP 认证应全部走 AUTH LOGIN 且成功: {main_auth:?}"),
            &mut failures,
        );

        // 7. 按需历史：冷启动窗口之外的第 5 封。
        let target_uid = HISTORY_TARGET_UID.to_string();
        let token = history_token("main", HISTORY_TARGET_UID);
        let local = rpc
            .call(
                "searchMessages",
                json!({ "query": token, "account": "main", "folder": "INBOX" }),
            )
            .await?;
        let local_hits_before = local["total"].as_u64().unwrap_or(u64::MAX);
        check(
            local_hits_before == 0,
            format!("第 5 封在冷启动窗口之外，本地检索应 0 命中，实际 {local_hits_before}"),
            &mut failures,
        );
        let search_started = Instant::now();
        let server_hits = rpc
            .call(
                "searchMessages",
                json!({ "query": token, "account": "main", "folder": "INBOX", "scope": "server" }),
            )
            .await?;
        let server_search_ms = search_started.elapsed().as_millis();
        let hit_uids: Vec<String> = server_hits["messages"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|entry| entry["uid"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        check(
            hit_uids == vec![target_uid.clone()],
            format!("服务器检索应恰好命中 UID {target_uid}，实际 {hit_uids:?}"),
            &mut failures,
        );
        let fetch_started = Instant::now();
        let fetched = rpc
            .call(
                "fetchServerMessage",
                json!({ "account": "main", "folder": "INBOX", "uid": target_uid }),
            )
            .await?;
        let fetch_ms = fetch_started.elapsed().as_millis();
        let expected_text = history_text("main", HISTORY_TARGET_UID).replace("\r\n", "\n");
        let fetched_body = fetched["message"]["body"].as_str().unwrap_or_default().replace("\r\n", "\n");
        let body_matches = fetched_body.trim() == expected_text.trim();
        check(
            body_matches,
            format!("按需拉回正文不一致:\n期望 {expected_text:?}\n实际 {fetched_body:?}"),
            &mut failures,
        );
        let persisted = rpc
            .call(
                "getMessage",
                json!({ "account": "main", "folder": "INBOX", "uid": target_uid }),
            )
            .await?;
        check(
            persisted["message"]["subject"]
                .as_str()
                .is_some_and(|subject| subject.contains(&token)),
            format!("按需拉回后应已落库: {persisted}"),
            &mut failures,
        );

        let final_status = runtime_of(&rpc.call("status", json!({})).await?);
        let exit_code = daemon.terminate().await?;
        check(
            exit_code == Some(0),
            format!("SIGTERM 后应以 0 退出，实际 {exit_code:?}"),
            &mut failures,
        );

        let report = json!({
            "bench": "maild footprint_bench",
            "measured_at_unix": now_unix(),
            "host": {
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
            },
            "daemon_binary": binary.display().to_string(),
            "setup": {
                "accounts": ACCOUNTS.iter().map(|(name, count)| json!({"account": name, "history": count})).collect::<Vec<_>>(),
                "initial_sync_limit": INITIAL_SYNC_LIMIT,
                "tls": "IMAP implicit TLS + SMTP implicit TLS (465 style), AUTH LOGIN only, loopback self-signed",
                "seed_ms": seed_ms,
            },
            "cold_start": {
                "socket_ready_ms": socket_ms,
                "all_online_ms": online_ms,
                "window_synced_and_indexed_ms": cold_ms,
                "stored_messages": cold_status.stored,
                "lowest_body_uid_fetched": lowest_body_uid,
                "notifications": cold_notifications,
                "rss_bytes": cold_status.rss,
                "phys_footprint_bytes": cold_status.footprint,
                "db_bytes": cold_status.db,
                "wal_bytes": cold_status.wal,
            },
            "steady_idle": {
                "rss_bytes": idle_after.rss,
                "phys_footprint_bytes": idle_after.footprint,
                "ps_rss_max_kib": idle_ps_rss_kib,
                "window_secs": idle_wall.as_secs_f64(),
                "cpu_time_ms": idle_cpu_ms,
                "cpu_percent": idle_cpu_pct,
                "cpu_source": cpu_source,
                "wakeups": idle_wakeups,
                "ps_pcpu_max": idle_ps_pcpu_max,
                "db_bytes": idle_after.db,
                "wal_bytes": idle_after.wal,
                "rss_bytes_before_window": idle_before.rss,
            },
            "live_idle_push": {
                "messages": wanted_all,
                "arrived": arrived_all,
                "latency_ms_p50": percentile(&latencies, 50.0),
                "latency_ms_p95": percentile(&latencies, 95.0),
                "latency_ms_max": latencies.last().copied().unwrap_or(f64::NAN),
                "otp_notifications": otp_notified,
                "otp_expected": expected_otp,
                "normal_messages": ACCOUNTS.len() * NORMAL_PER_ACCOUNT,
                "normal_notifications": normal_notified,
                "normal_expected": expected_normal,
                "total_notifications": live_notifications,
                "after": {
                    "rss_bytes": after_live.rss,
                    "phys_footprint_bytes": after_live.footprint,
                    "ps_rss_kib": after_live_ps.rss_kib,
                    "phys_footprint_growth_bytes": footprint_growth,
                    "db_bytes": after_live.db,
                    "wal_bytes": after_live.wal,
                },
            },
            "daemon_send_smtps465": {
                "roundtrip_ms": daemon_send_roundtrip_ms,
                "auth_log": main_auth.iter().map(|(m, ok)| json!({"mechanism": m, "ok": ok})).collect::<Vec<_>>(),
            },
            "on_demand_history": {
                "uid": HISTORY_TARGET_UID,
                "local_hits_before": local_hits_before,
                "server_search_ms": server_search_ms,
                "server_hits": hit_uids,
                "fetch_ms": fetch_ms,
                "body_matches": body_matches,
            },
            "final": {
                "rss_bytes": final_status.rss,
                "phys_footprint_bytes": final_status.footprint,
                "stored_messages": final_status.stored,
                "db_bytes": final_status.db,
                "wal_bytes": final_status.wal,
                "sigterm_exit_code": exit_code,
            },
            "failures": failures,
        });

        if !json_only {
            println!("maild footprint_bench（release 子进程，{} {}）", std::env::consts::OS, std::env::consts::ARCH);
            println!("| 指标 | 实测 |");
            println!("| --- | --- |");
            println!("| 冷启动：socket 就绪 | {socket_ms} ms |");
            println!("| 冷启动：3 账户全部 online | {} ms |", online_ms.unwrap_or(0));
            println!("| 冷启动：3×{INITIAL_SYNC_LIMIT} 封有界同步 + FTS 收敛 | {cold_ms} ms |");
            println!("| 冷启动后 RSS / phys_footprint | {:.1} MiB / {:.1} MiB |", mib(cold_status.rss), mib(cold_status.footprint));
            println!("| 稳态空闲 RSS / phys_footprint | {:.1} MiB / {:.1} MiB |", mib(idle_after.rss), mib(idle_after.footprint));
            println!("| 稳态空闲 ps RSS 峰值 | {:.1} MiB |", idle_ps_rss_kib as f64 / 1024.0);
            println!("| 5 秒空闲 CPU（{cpu_source} 累计 CPU 时间 / 墙钟） | {idle_cpu_pct:.4}%（{idle_cpu_ms:.2} ms / {:.2} s） |", idle_wall.as_secs_f64());
            if let Some(wakeups) = idle_wakeups {
                println!("| 5 秒空闲唤醒次数（idle + interrupt wakeups） | {wakeups} |");
            }
            println!("| 5 秒空闲 ps pcpu 峰值 | {idle_ps_pcpu_max:.1}% |");
            println!("| 实时推送到达 | {arrived_all}/{wanted_all} |");
            println!("| SMTPS 投递 → message/received P50 / P95 / max | {:.1} / {:.1} / {:.1} ms |", percentile(&latencies, 50.0), percentile(&latencies, 95.0), latencies.last().copied().unwrap_or(f64::NAN));
            println!("| 验证码通知（期望 {expected_otp}） | {otp_notified} |");
            println!("| 普通信 {} 封 → 限流后通知（期望 {expected_normal}） | {normal_notified} |", ACCOUNTS.len() * NORMAL_PER_ACCOUNT);
            println!("| 注入后 RSS / phys_footprint | {:.1} MiB / {:.1} MiB（footprint Δ {:+.2} MiB） |", mib(after_live.rss), mib(after_live.footprint), footprint_growth as f64 / (1024.0 * 1024.0));
            println!("| 注入后 DB / WAL | {:.2} MiB / {:.2} MiB |", mib(after_live.db), mib(after_live.wal));
            println!("| 守护进程 send（SMTPS 465 + AUTH LOGIN）→ IDLE 收回 | {} ms |", daemon_send_roundtrip_ms.map(|v| v.to_string()).unwrap_or_else(|| "未收回".into()));
            println!("| 冷启动正文抓取最低 UID（窗口外不得抓取） | {} |", lowest_body_uid.iter().map(|(a, u)| format!("{a}={u}")).collect::<Vec<_>>().join(", "));
            println!("| 服务器检索第 5 封 / 按需拉回 | {server_search_ms} ms / {fetch_ms} ms（正文一致: {body_matches}） |");
            println!("| SIGTERM 退出码 | {exit_code:?} |");
            println!();
        }
        println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
        drop(servers);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!("{} 项验收未通过:\n- {}", failures.len(), failures.join("\n- ")))
        }
    }
}
