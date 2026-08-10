// --- Queue layout tokens (shared by header, rows, and width budgets) ---
// Keep metric columns tight so Name keeps the bulk of the row width.
/// Fits short-date forms like `08/10/2026` / locale variants (was 68 for relative-only).
pub(crate) const COL_DATE_W: f32 = 80.0;
pub(crate) const COL_SPEED_W: f32 = 76.0;
pub(crate) const COL_ETA_W: f32 = 56.0;
pub(crate) const COL_SIZE_W: f32 = 92.0;
/// Single overflow control — no multi-icon action strip.
pub(crate) const COL_ACTIONS_W: f32 = 40.0;
/// Status color dot beside the filename (tooltip shows the full label).
pub(crate) const STATUS_DOT: f32 = 9.0;
/// Keep at least this much list height when the detail panel is open.
pub(crate) const LIST_MIN_H: f32 = 140.0;
/// Hard cap for the selected-job detail panel (also clamped vs viewport).
pub(crate) const DETAIL_MAX_H: f32 = 280.0;
pub(crate) const DETAIL_MIN_CAP: f32 = 180.0;

/// Which fixed metric columns fit in the main content area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QueueColumns {
    pub date: bool,
    pub speed: bool,
    pub eta: bool,
}

impl QueueColumns {
    /// Progressive collapse so Name stays dominant; metrics hide first when tight.
    pub(crate) fn from_main_width(main_w: f32) -> Self {
        // With compact metrics + overflow actions, full grid fits sooner.
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

