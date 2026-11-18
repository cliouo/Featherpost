//! 与操作系统打交道的三件小事（ADR-0033 决策 6/8）：交系统浏览器、保存文件名清洗、原子写文件。
//! 纯函数部分有单测；`open_external` 只在命令层调用。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 只把 `http`/`https` 交给系统浏览器。调用方（命令层）已按链接表分类过一次，这里再校验一次——
/// 两道门都不信任 webview 传来的字符串。
pub fn open_external(url: &str) -> Result<(), String> {
    let lower = url.trim().to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("只有 http/https 链接会交给系统浏览器".into());
    }
    if url.chars().any(|ch| ch.is_control()) {
        return Err("链接含控制字符".into());
    }
    let mut command = launcher(url.trim());
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法启动系统浏览器: {error}"))
}

/// 固定帮助页（服务商官方设置说明等）交系统浏览器：只收 `https`、必须有主机名、不带
/// userinfo、限长。邮件正文里的链接不走这里——那条路只认文档令牌（`ui_open_link`）。
pub fn open_web_url(url: &str) -> Result<(), String> {
    let url = validate_web_url(url)?;
    open_external(&url)
}

/// [`open_web_url`] 的纯校验部分（便于单测）。返回规范化后的 URL。
pub fn validate_web_url(url: &str) -> Result<String, String> {
    if url.len() > 2048 || url.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return Err("链接过长或含空白/控制字符".into());
    }
    let parsed = tauri::Url::parse(url).map_err(|_| "链接无效".to_string())?;
    if parsed.scheme() != "https" {
        return Err("只打开 https 链接".into());
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err("链接缺少主机名".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("链接不得携带用户信息".into());
    }
    Ok(parsed.to_string())
}

#[cfg(target_os = "macos")]
fn launcher(url: &str) -> Command {
    let mut command = Command::new("open");
    command.arg("--").arg(url);
    command
}

#[cfg(target_os = "linux")]
fn launcher(url: &str) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(url);
    command
}

#[cfg(target_os = "windows")]
fn launcher(url: &str) -> Command {
    // 不经 cmd /c start：`&` 等字符会被 shell 解释。
    let mut command = Command::new("rundll32");
    command.arg("url.dll,FileProtocolHandler").arg(url);
    command
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn launcher(_url: &str) -> Command {
    Command::new("false")
}

/// 保存面板的建议文件名：去掉路径分隔与控制字符、首尾空白与点；空则按 part 序号生成。
pub fn safe_filename(filename: &str, part_index: u32) -> String {
    let cleaned: String = filename
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' => '_',
            ch if ch.is_control() => ' ',
            ch => ch,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        format!("附件-{part_index}")
    } else {
        let mut out = String::new();
        for ch in trimmed.chars() {
            if out.len() + ch.len_utf8() > 200 {
                break;
            }
            out.push(ch);
        }
        out
    }
}

/// 写到同目录临时文件、fsync，再 `rename` 到目标。面板已替用户确认过重名，覆盖是用户的决定；
/// 写失败时临时文件被清理、目标一个字节不动。
pub fn write_atomically(target: &Path, bytes: &[u8]) -> std::io::Result<PathBuf> {
    use std::io::Write as _;
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let temp = parent.join(format!(
        ".maild-save-{}-{}.tmp",
        std::process::id(),
        crate::documents::new_token()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map(|_| target.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_urls_must_be_plain_https() {
        assert_eq!(
            validate_web_url("https://service.mail.qq.com/detail/0/75").unwrap(),
            "https://service.mail.qq.com/detail/0/75"
        );
        for bad in [
            "http://example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://user:pw@example.com/",
            "https://exa mple.com",
            "https://",
            "mailto:a@b.c",
        ] {
            assert!(validate_web_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_web_links_reach_the_system_browser() {
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "mailto:a@b",
            "ftp://x",
            "",
            "http://a\nb",
        ] {
            assert!(open_external(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn filenames_are_cleaned_and_never_empty() {
        assert_eq!(safe_filename("季度报告.pdf", 2), "季度报告.pdf");
        // 分隔符换成下划线，首尾的点去掉：不可能再是路径。
        assert_eq!(safe_filename("../../etc/passwd", 2), "_.._etc_passwd");
        assert_eq!(safe_filename("  .hidden  ", 2), "hidden");
        assert_eq!(safe_filename("a\u{0}b\u{7}c", 2), "a b c");
        assert_eq!(safe_filename("", 5), "附件-5");
        assert_eq!(safe_filename("...", 5), "附件-5");
        assert!(safe_filename(&"字".repeat(300), 1).len() <= 200);
    }

    #[test]
    fn atomic_write_lands_the_bytes_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.bin");
        let written = write_atomically(&target, b"hello").unwrap();
        assert_eq!(written, target);
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".maild-save-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // 目标目录不存在：失败且不留临时文件。
        let missing = dir.path().join("nope").join("out.bin");
        assert!(write_atomically(&missing, b"x").is_err());
        assert!(!dir.path().join("nope").exists());
    }
}
