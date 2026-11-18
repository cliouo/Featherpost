#!/bin/bash
# Stalwart 0.15.5 本地启动脚本（maild 真服务器交叉验证用）
# 用法：./start.sh
set -e

DIR="$(cd "$(dirname "$0")" && pwd)"
DATA="/tmp/swmail"

# 下载二进制（断点续传，跳过已存在）
if [ ! -f "$DIR/stalwart" ]; then
  echo "=== 下载 Stalwart 0.15.5 ==="
  curl -L -C - -o "$DIR/stalwart.tar.gz" \
    "https://github.com/stalwartlabs/stalwart/releases/download/v0.15.5/stalwart-aarch64-apple-darwin.tar.gz"
  tar xzf "$DIR/stalwart.tar.gz" stalwart -C "$DIR"
  rm -f "$DIR/stalwart.tar.gz"
  chmod +x "$DIR/stalwart"
fi

# 生成自签证书
if [ ! -f "$DATA/tls/cert.pem" ]; then
  mkdir -p "$DATA/tls"
  openssl req -x509 -newkey rsa:2048 -keyout "$DATA/tls/key.pem" -out "$DATA/tls/cert.pem" \
    -days 3650 -nodes -subj "/CN=maild.test" \
    -addext "subjectAltName=DNS:maild.test,DNS:localhost,IP:127.0.0.1" 2>/dev/null
fi

# 写配置
mkdir -p "$DATA"
cat > "$DATA/config.toml" <<'EOF'
[server]
hostname = "maild.test"

[server.listener.smtp]
bind = "127.0.0.1:1025"
protocol = "smtp"

[server.listener.imap]
bind = "127.0.0.1:1143"
protocol = "imap"

[storage]
data = "rocksdb"
fts = "rocksdb"
blob = "rocksdb"
lookup = "rocksdb"
directory = "memory"

[store.rocksdb]
type = "rocksdb"
path = "/tmp/swmail/data"
compression = "lz4"

[directory.memory]
type = "memory"

[[directory.memory.principals]]
name = "dev"
class = "individual"
secret = "devpass"
email = ["dev@maild.test"]

[tracer.stdout]
type = "stdout"
level = "info"
ansi = false
enable = true

[certificate."default"]
cert = "%{file:/tmp/swmail/tls/cert.pem}%"
private-key = "%{file:/tmp/swmail/tls/key.pem}%"
default = true

[session.auth]
require = false

[spam-filter]
enable = false
EOF

echo "=== 启动 Stalwart ==="
echo "  SMTP: 127.0.0.1:1025"
echo "  IMAP: 127.0.0.1:1143 (STARTTLS)"
echo "  账号: dev / devpass → dev@maild.test"
echo ""
exec "$DIR/stalwart" -c "$DATA/config.toml"