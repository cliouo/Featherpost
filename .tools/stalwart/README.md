# Stalwart 真服务器交叉验证

用独立实现的 Stalwart Mail Server（0.15.5，Rust 生产级邮件服务器）交叉验证 maild 的
`transport`（SMTP）与 `receive`（IMAP STARTTLS + IDLE）客户端，消除 hermetic 假服务器的
同源实现偏见。

## 一键启动

```sh
cd .tools/stalwart
./start.sh
```

启动后：
- **SMTP**：127.0.0.1:1025（STARTTLS、自签证书、免认证本地投递，`session.auth.require = false`）
- **IMAP**：127.0.0.1:1143（STARTTLS，自签证书；账号 `dev` / `devpass`，邮箱 `dev@maild.test`）
- spam 过滤关闭（`spam-filter.enable = false`，否则测试邮件会被 Pyzor 判 spam 进 Junk）

## 手动步骤（start.sh 做了什么）

1. 下载二进制（断点续传，GitHub release 慢）：
   ```sh
   curl -L -C - -o stalwart015.tar.gz \
     https://github.com/stalwartlabs/stalwart/releases/download/v0.15.5/stalwart-aarch64-apple-darwin.tar.gz
   tar xzf stalwart015.tar.gz stalwart && chmod +x stalwart
   ```
   > ⚠️ 用 **0.15.5**（TOML 配置）。**0.16+ 配置层已重构**（config.json + JMAP 管理 +
   > 独立 CLI），无 TOML，本地快速验证反而更繁琐。

2. 生成自签证书：
   ```sh
   mkdir -p /tmp/swmail/tls && cd /tmp/swmail/tls
   openssl req -x509 -newkey rsa:2048 -keyout key.pem -out cert.pem \
     -days 3650 -nodes -subj "/CN=maild.test" \
     -addext "subjectAltName=DNS:maild.test,DNS:localhost,IP:127.0.0.1"
   ```

3. 写 `/tmp/swmail/config.toml`（见同目录 `stalwart.toml` 模板）。

4. 启动：
   ```sh
   ./stalwart -c /tmp/swmail/config.toml
   ```

## 跑交叉验证测试

```sh
cargo test -p maild --test real_server -- --ignored --nocapture
```

环境变量可覆盖端点：`REAL_IMAP_HOST/PORT/USER/PASS/TLS/ACCEPT_INVALID_CERTS`、
`REAL_SMTP_HOST/PORT/USER/PASS/TLS/ACCEPT_INVALID_CERTS/FROM/TO`。本地自签环境默认
允许无效证书；真实服务必须把 IMAP/SMTP 的 `ACCEPT_INVALID_CERTS` 都设为 `false`。
密码、用户名和地址会按 TOML 字符串转义，不会写入仓库或测试输出。

链路：`transport.send()`（SMTP STARTTLS）→ Stalwart 收信 → receive（IMAP STARTTLS + IDLE）
推送 → 抓取 → otp 提取验证码 → notify 通知。

## 关键踩坑记录

| 现象 | 原因 | 解决 |
|---|---|---|
| IMAP 明文 `LOGIN` 被拒 | Stalwart 默认禁明文端口登录 | 走 STARTTLS 或 993 隐式 TLS |
| SMTP `MAIL FROM` → `503 authenticate first` | 默认要求认证，且只开 OAuth | `[session.auth] require = false` |
| 邮件 queued 但 INBOX `0 EXISTS` | spam 过滤（Pyzor 远程判定）投进 Junk | `[spam-filter] enable = false` |
| 0.16 无 TOML、需 `stalwart-cli` + JMAP | 0.16 配置层重构 | 锁定 0.15.5 |
