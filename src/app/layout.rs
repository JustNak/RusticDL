pub(crate) const COL_DATE_W: f32 = 80.0;
pub(crate) const COL_SPEED_W: f32 = 76.0;
pub(crate) const COL_ETA_W: f32 = 56.0;
pub(crate) const COL_SIZE_W: f32 = 92.0;
pub(crate) const COL_ACTIONS_W: f32 = 40.0;
pub(crate) const FILE_ICON_W: f32 = 24.0;
pub(crate) const STATUS_BADGE: f32 = 8.0;
pub(crate) const STATUS_DOT: f32 = FILE_ICON_W;
pub(crate) const LIST_MIN_H: f32 = 140.0;
pub(crate) const DETAIL_MAX_H: f32 = 280.0;
pub(crate) const DETAIL_MIN_CAP: f32 = 180.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueueColumns {
    pub date: bool,
    pub speed: bool,
    pub eta: bool,
}

impl QueueColumns {
    pub(crate) fn from_main_width(main_w: f32) -> Self {
        if main_w >= 780.0 {
            Self {
                date: true,
                speed: true,
                eta: true,
            }
        } else if main_w >= 680.0 {
            Self {
                date: true,
                speed: true,
                eta: false,
            }
        } else if main_w >= 600.0 {
            Self {
                date: false,
                speed: true,
                eta: false,
            }
        } else {
            Self {
                date: false,
                speed: false,
                eta: false,
            }
        }
    }
}

/// Sidebar sits opposite the native window controls so traffic lights / close
/// buttons are never drawn over the brand column.
pub(crate) fn sidebar_on_right() -> bool {
    native_window_controls_on_left()
}

fn native_window_controls_on_left() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(target_os = "windows")]
    {
        false
    }
    #[cfg(target_os = "linux")]
    {
        linux_window_controls_on_left()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}

/// GTK / GNOME `button-layout` / `gtk-decoration-layout`: `left:right`.
/// Controls are on the left when that cluster owns close / min / max.
pub(crate) fn decoration_layout_controls_on_left(raw: &str) -> bool {
    let layout = raw
        .trim()
        .trim_start_matches("@s")
        .trim()
        .trim_matches('\'')
        .trim_matches('"')
        .trim();
    if layout.is_empty() {
        return false;
    }
    let (left, right) = layout.split_once(':').unwrap_or(("", layout));
    let left_close = left.split(',').map(str::trim).any(|b| b == "close");
    let right_close = right.split(',').map(str::trim).any(|b| b == "close");
    if left_close != right_close {
        return left_close;
    }
    fn has_control(side: &str) -> bool {
        side.split(',')
            .map(str::trim)
            .any(|b| matches!(b, "close" | "minimize" | "maximize" | "min" | "max"))
    }
    has_control(left) && !has_control(right)
}

#[cfg(target_os = "linux")]
fn linux_window_controls_on_left() -> bool {
    use std::sync::OnceLock;
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        if crate::hyprland::is_hyprland() {
            return false;
        }
        if let Some(layout) = gnome_button_layout() {
            return decoration_layout_controls_on_left(&layout);
        }
        if let Some(layout) = gtk_decoration_layout() {
            return decoration_layout_controls_on_left(&layout);
        }
        xdg_desktop_prefers_left_controls()
    })
}

#[cfg(target_os = "linux")]
fn gnome_button_layout() -> Option<String> {
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.wm.preferences", "button-layout"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(target_os = "linux")]
fn gtk_decoration_layout() -> Option<String> {
    let config = dirs::config_dir()?;
    for name in ["gtk-4.0/settings.ini", "gtk-3.0/settings.ini"] {
        let Ok(text) = std::fs::read_to_string(config.join(name)) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("gtk-decoration-layout=") {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn xdg_desktop_prefers_left_controls() -> bool {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    desktop
        .split(':')
        .map(|part| part.trim().to_ascii_lowercase())
        .any(|part| matches!(part.as_str(), "unity" | "pantheon" | "elementary"))
}

#[cfg(test)]
mod chrome_side_tests {
    use super::*;

    #[test]
    fn gnome_right_controls_keep_sidebar_left() {
        assert!(!decoration_layout_controls_on_left(
            "'appmenu:minimize,maximize,close'"
        ));
        assert!(!decoration_layout_controls_on_left(
            ":minimize,maximize,close"
        ));
        assert!(!decoration_layout_controls_on_left(
            "icon:minimize,maximize,close"
        ));
    }

    #[test]
    fn ubuntu_and_elementary_left_controls_move_sidebar_right() {
        assert!(decoration_layout_controls_on_left(
            "'close,minimize,maximize:'"
        ));
        assert!(decoration_layout_controls_on_left("close:maximize"));
        assert!(decoration_layout_controls_on_left(
            "close,minimize,maximize:"
        ));
    }

    #[test]
    fn empty_or_unknown_layout_defaults_to_right_controls() {
        assert!(!decoration_layout_controls_on_left(""));
        assert!(!decoration_layout_controls_on_left("   "));
        assert!(!decoration_layout_controls_on_left("appmenu:"));
    }

    #[test]
    fn platform_default_sidebar_side() {
        #[cfg(target_os = "macos")]
        assert!(sidebar_on_right());
        #[cfg(target_os = "windows")]
        assert!(!sidebar_on_right());
    }
}
