//! Studio's interface is Chinese, including the native menu outside the webview.

use tauri::menu::{Menu, MenuItemKind};
use tauri::{AppHandle, Runtime};

pub(super) fn menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    // Keep Tauri's IDs, native selectors, accelerators, Window/Help roles and About metadata.
    // Only the visible labels change; in particular, Quit still follows the normal save guard.
    let menu = Menu::default(app)?;
    localize(menu.items()?)?;
    Ok(menu)
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
