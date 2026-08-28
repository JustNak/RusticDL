use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppTheme {
    #[default]
    Light,
    Dark,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccentPreset {
    /// Keep the built-in theme primary (no tint override).
    #[default]
    Default,
    Blue,
    Cyan,
    Emerald,
    Amber,
    Rose,
    Violet,
    Orange,
    Slate,
    Custom,
}

impl AccentPreset {
    pub const ALL: [AccentPreset; 10] = [
        AccentPreset::Default,
        AccentPreset::Blue,
        AccentPreset::Cyan,
        AccentPreset::Emerald,
        AccentPreset::Amber,
        AccentPreset::Rose,
        AccentPreset::Violet,
        AccentPreset::Orange,
        AccentPreset::Slate,
        AccentPreset::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Blue => "Blue",
            Self::Cyan => "Cyan",
            Self::Emerald => "Emerald",
            Self::Amber => "Amber",
            Self::Rose => "Rose",
            Self::Violet => "Violet",
            Self::Orange => "Orange",
            Self::Slate => "Slate",
            Self::Custom => "Custom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UiDensity {
    #[default]
    Comfortable,
    Compact,
}

impl UiDensity {
    pub const ALL: [UiDensity; 2] = [UiDensity::Comfortable, UiDensity::Compact];

    pub fn label(self) -> &'static str {
        match self {
            Self::Comfortable => "Comfortable",
            Self::Compact => "Compact",
        }
    }

    pub fn row_h(self) -> f32 {
        match self {
            Self::Comfortable => 52.0,
            Self::Compact => 42.0,
        }
    }

    pub fn sidebar_w(self) -> f32 {
        match self {
            Self::Comfortable => 220.0,
            Self::Compact => 192.0,
        }
    }

    pub fn settings_pad(self) -> f32 {
        match self {
            Self::Comfortable => 24.0,
            Self::Compact => 16.0,
        }
    }

    pub fn font_size(self) -> f32 {
        match self {
            Self::Comfortable => 16.0,
            Self::Compact => 14.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CornerRadiusScale {
    Sharp,
    #[default]
    Default,
    Soft,
}

impl CornerRadiusScale {
    pub const ALL: [CornerRadiusScale; 3] = [
        CornerRadiusScale::Sharp,
        CornerRadiusScale::Default,
        CornerRadiusScale::Soft,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sharp => "Sharp",
            Self::Default => "Default",
            Self::Soft => "Soft",
        }
    }

    /// (radius, radius_lg) in logical px.
    pub fn radii(self) -> (f32, f32) {
        match self {
            Self::Sharp => (2.0, 4.0),
            Self::Default => (6.0, 8.0),
            Self::Soft => (10.0, 14.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProgressStyle {
    #[default]
    Solid,
    Soft,
    Glow,
    Segmented,
}

impl ProgressStyle {
    pub const ALL: [ProgressStyle; 4] = [
        ProgressStyle::Solid,
        ProgressStyle::Soft,
        ProgressStyle::Glow,
        ProgressStyle::Segmented,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Soft => "Soft",
            Self::Glow => "Glow",
            Self::Segmented => "Segmented",
        }
    }
}

/// Hard floor for *effective* window alpha when transparency is maxed.
/// Slider 100% still keeps the window at least this opaque.
pub const MIN_WINDOW_OPACITY: u8 = 75;
pub const MAX_WINDOW_TRANSPARENCY: u8 = 100;
pub const MAX_NOISE_INTENSITY: u8 = 100;
pub const MAX_VIGNETTE_INTENSITY: u8 = 100;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_and_radius_tokens() {
        assert!(UiDensity::Compact.row_h() < UiDensity::Comfortable.row_h());
        assert!(UiDensity::Compact.sidebar_w() < UiDensity::Comfortable.sidebar_w());
        let (sharp, _) = CornerRadiusScale::Sharp.radii();
        let (soft, soft_lg) = CornerRadiusScale::Soft.radii();
        assert!(sharp < soft);
        assert!(soft < soft_lg);
    }
}
