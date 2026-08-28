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
