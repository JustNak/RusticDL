use serde::{Deserialize, Serialize};

/// Default first-run window size (logical px). Matches the designed layout:
/// sidebar + full metric columns + detail panel with comfortable breathing room.
pub const DEFAULT_WINDOW_WIDTH: f32 = 1120.0;
pub const DEFAULT_WINDOW_HEIGHT: f32 = 720.0;
/// Matches `window_min_size` in `main.rs` (progressive column-collapse floor).
pub const MIN_WINDOW_WIDTH: f32 = 960.0;
pub const MIN_WINDOW_HEIGHT: f32 = 600.0;
const MAX_WINDOW_DIM: f32 = 10_000.0;

/// Persisted main-window geometry (logical pixels).
///
/// - Fresh install: centered `DEFAULT_WINDOW_*` size, not maximized.
/// - After the user resizes/moves: restored on next launch (including maximized).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowLayout {
    pub width: f32,
    pub height: f32,
    /// Top-left origin in screen coordinates; `None` means center on the cursor's
    /// monitor work area (fallback: host window monitor, then primary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    #[serde(default)]
    pub maximized: bool,
}

impl Default for WindowLayout {
    fn default() -> Self {
        Self {
            width: DEFAULT_WINDOW_WIDTH,
            height: DEFAULT_WINDOW_HEIGHT,
            x: None,
            y: None,
            maximized: false,
        }
    }
}

impl WindowLayout {
    pub fn sanitize(&mut self) {
        if !self.width.is_finite() {
            self.width = DEFAULT_WINDOW_WIDTH;
        }
        if !self.height.is_finite() {
            self.height = DEFAULT_WINDOW_HEIGHT;
        }
        self.width = self.width.clamp(MIN_WINDOW_WIDTH, MAX_WINDOW_DIM);
        self.height = self.height.clamp(MIN_WINDOW_HEIGHT, MAX_WINDOW_DIM);
        if let Some(x) = self.x {
            if !x.is_finite() {
                self.x = None;
            }
        }
        if let Some(y) = self.y {
            if !y.is_finite() {
                self.y = None;
            }
        }
        // Position is all-or-nothing so restore never anchors only one axis.
        if self.x.is_none() || self.y.is_none() {
            self.x = None;
            self.y = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_layout_sanitize_clamps_and_defaults() {
        let mut layout = WindowLayout {
            width: 100.0,
            height: f32::NAN,
            x: Some(f32::INFINITY),
            y: Some(40.0),
            maximized: true,
        };
        layout.sanitize();
        assert_eq!(layout.width, MIN_WINDOW_WIDTH);
        assert_eq!(layout.height, DEFAULT_WINDOW_HEIGHT);
        assert!(layout.x.is_none());
        assert!(layout.y.is_none());
        assert!(layout.maximized);
    }
}
