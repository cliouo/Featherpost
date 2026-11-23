//! 原生菜单跟随界面语言（ADR-0048 决策 8）：属于默认前端而不是内核，语言取自同一份 daemon 偏好，
//! 用与 webview 相同的协商规则（`maild_l10n::negotiate`），不另读系统设置。
//!
//! 中文用下面的对照表逐项改写 Tauri 默认菜单的可见文字；英文直接用 Tauri 默认的英文标签。

use maild_l10n::Locale;
use tauri::menu::{Menu, MenuItemKind};
use tauri::{AppHandle, Runtime};

/// 启动时的菜单：还没拿到偏好，与 webview 一样先用基础语言（今天的行为）。
pub(super) fn menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    menu_for(app, Locale::default())
}

fn menu_for<R: Runtime>(app: &AppHandle<R>, locale: Locale) -> tauri::Result<Menu<R>> {
    // Keep Tauri's IDs, native selectors, accelerators, Window/Help roles and About metadata.
    // Only the visible labels change; in particular, Quit still follows the normal save guard.
    let menu = Menu::default(app)?;
    if locale == Locale::ZhHans {
        localize(menu.items()?)?;
    }
    Ok(menu)
}

/// 偏好变化后整份重建并替换应用菜单。重建而不是就地改名：英文标签直接来自 Tauri 默认菜单，
/// 不需要一张反向对照表。排到主线程执行，调用方不阻塞。
pub(super) fn apply(app: &AppHandle, locale: Locale) {
    let handle = app.clone();
    let scheduled = app.run_on_main_thread(move || {
        let result = menu_for(&handle, locale).and_then(|menu| handle.set_menu(menu).map(drop));
        if let Err(error) = result {
            eprintln!("[maild-ui] 无法按 {} 重建原生菜单: {error}", locale.tag());
        }
    });
    if let Err(error) = scheduled {
        eprintln!("[maild-ui] 无法排程原生菜单重建: {error}");
    }
}

fn localize<R: Runtime>(items: Vec<MenuItemKind<R>>) -> tauri::Result<()> {
    for item in items {
        match item {
            MenuItemKind::Submenu(submenu) => {
                if let Some(label) = menu_label(&submenu.text()?) {
                    submenu.set_text(label)?;
                }
                localize(submenu.items()?)?;
            }
            MenuItemKind::Predefined(item) => {
                if let Some(label) = menu_label(&item.text()?) {
                    item.set_text(label)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// Source strings are from the locked Tauri/muda standard menu, not provider or user data.
fn menu_label(value: &str) -> Option<String> {
    let label = match value {
        "File" => "文件",
        "Edit" => "编辑",
        "View" => "显示",
        "Window" => "窗口",
        "Help" => "帮助",
        "Services" => "服务",
        "Undo" => "撤销",
        "Redo" => "重做",
        "Cut" => "剪切",
        "Copy" => "复制",
        "Paste" => "粘贴",
        "Select All" => "全选",
        "Minimize" => "最小化",
        "Zoom" | "Maximize" => "缩放",
        "Close" | "Close Window" => "关闭窗口",
        "Toggle Full Screen" => "切换全屏",
        "Enter Full Screen" => "进入全屏",
        "Exit Full Screen" => "退出全屏",
        "Hide Others" => "隐藏其他",
        "Show All" => "全部显示",
        "About" => "关于",
        "Hide" => "隐藏",
        "Quit" => "退出",
        _ => {
            for (prefix, translated) in
                [("About ", "关于 "), ("Hide ", "隐藏 "), ("Quit ", "退出 ")]
            {
                if let Some(name) = value.strip_prefix(prefix) {
                    return Some(format!("{translated}{name}"));
                }
            }
            return None;
        }
    };
    Some(label.to_owned())
}

#[cfg(test)]
mod tests {
    use super::menu_label;

    #[test]
    fn localizes_standard_labels_without_rewriting_names_or_unknown_items() {
        assert_eq!(menu_label("Edit").as_deref(), Some("编辑"));
        assert_eq!(menu_label("Hide Others").as_deref(), Some("隐藏其他"));
        assert_eq!(
            menu_label("Toggle Full Screen").as_deref(),
            Some("切换全屏")
        );
        for (source, expected) in [
            ("About Featherpost Studio", "关于 Featherpost Studio"),
            ("Hide Featherpost Studio", "隐藏 Featherpost Studio"),
            ("Quit Featherpost Studio", "退出 Featherpost Studio"),
        ] {
            assert_eq!(menu_label(source).as_deref(), Some(expected));
        }
        assert_eq!(menu_label("Featherpost Studio"), None);
        assert_eq!(menu_label("Custom operation"), None);
        assert_eq!(menu_label(""), None);
        assert_eq!(menu_label("编辑"), None);
    }
}
