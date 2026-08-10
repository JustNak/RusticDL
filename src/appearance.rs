//! Appearance pipeline: resolve theme mode, tint accents, apply surface opacity,
//! window translucency, and film-grain texture.

use std::sync::{Arc, OnceLock};

use gpui::{
    hsla, px, App, Hsla, RenderImage, Window, WindowAppearance, WindowBackgroundAppearance,
};
use gpui_component::{ColorName, Colorize, Theme, ThemeMode};
use image::{Frame, Rgba, RgbaImage};
use smallvec::SmallVec;

use crate::settings::{
    AccentPreset, AppTheme, Settings, MAX_NOISE_INTENSITY, MAX_VIGNETTE_INTENSITY,
    MAX_WINDOW_TRANSPARENCY, MIN_WINDOW_OPACITY,
};

/// Map a user theme choice to a concrete light/dark mode.
pub fn resolve_theme_mode(theme: AppTheme, window: Option<&Window>, cx: &App) -> ThemeMode {
    match theme {
        AppTheme::Light => ThemeMode::Light,
        AppTheme::Dark => ThemeMode::Dark,
        AppTheme::System => {
            let appearance = window
                .map(|w| w.appearance())
                .unwrap_or_else(|| cx.window_appearance());
            match appearance {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
                WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
            }
        }
    }
}

/// Map the transparency slider (0–100) to window alpha with a hard floor.
///
/// - `0` → fully opaque (`1.0`) — default, no transparency
/// - `100` → [`MIN_WINDOW_OPACITY`]% alpha (never fully invisible)
/// - values in between interpolate linearly
pub fn effective_window_opacity_alpha(transparency_pct: u8) -> f32 {
    let t = (transparency_pct.min(MAX_WINDOW_TRANSPARENCY) as f32) / 100.0;
    let floor = (MIN_WINDOW_OPACITY as f32) / 100.0;
    // alpha = 1 at t=0, floor at t=1
    1.0 - t * (1.0 - floor)
}

/// Accent Hsla for a preset (or custom HSL). `None` = keep stock theme primary.
pub fn resolve_accent_color(settings: &Settings, is_dark: bool) -> Option<Hsla> {
    let scale = if is_dark { 400 } else { 600 };
    let color = match settings.accent_preset {
        AccentPreset::Default => return None,
        AccentPreset::Blue => ColorName::Blue.scale(scale),
        AccentPreset::Cyan => ColorName::Cyan.scale(scale),
        AccentPreset::Emerald => ColorName::Emerald.scale(scale),
        AccentPreset::Amber => ColorName::Amber.scale(scale),
        AccentPreset::Rose => ColorName::Rose.scale(scale),
        AccentPreset::Violet => ColorName::Violet.scale(scale),
        AccentPreset::Orange => ColorName::Orange.scale(scale),
        AccentPreset::Slate => ColorName::Gray.scale(if is_dark { 300 } else { 700 }),
        AccentPreset::Custom => {
            return Some(custom_accent_hsla(
                settings.accent_hue,
                settings.accent_saturation,
                settings.accent_lightness,
            ));
        }
    };
    Some(color)
}

/// Resolve the user-tuned custom accent (HSL channels in UI ranges).
pub fn custom_accent_hsla(hue: f32, saturation: f32, lightness: f32) -> Hsla {
    let h = hue.rem_euclid(360.0);
    let s = saturation.clamp(0.0, 100.0);
    let l = lightness.clamp(0.0, 100.0);
    gpui_component::hsl(h, s, l)
}

/// Swatch color for the settings accent picker (always vivid enough to read).
///
/// `stock_primary` is the live theme primary — used for **Default**, which does
/// not override accents (`resolve_accent_color` → `None`). On dark themes that
/// is often near-white (same as the Primary button), not the Blue preset.
pub fn accent_swatch_color(
    preset: AccentPreset,
    custom_hue: f32,
    custom_sat: f32,
    custom_light: f32,
    stock_primary: Hsla,
) -> Hsla {
    match preset {
        AccentPreset::Default => stock_primary,
        AccentPreset::Custom => custom_accent_hsla(custom_hue, custom_sat, custom_light),
        other => {
            let mut probe = Settings::default();
            probe.accent_preset = other;
            // Probe as dark so swatches stay vivid in the picker regardless of mode.
            resolve_accent_color(&probe, true).unwrap_or(stock_primary)
        }
    }
}

fn readable_on(primary: Hsla) -> Hsla {
    if primary.l > 0.55 {
        hsla(0.0, 0.0, 0.12, 1.0)
    } else {
        hsla(0.0, 0.0, 0.98, 1.0)
    }
}

fn with_alpha(color: Hsla, a: f32) -> Hsla {
    Hsla {
        a: a.clamp(0.0, 1.0),
        ..color
    }
}

fn apply_accent(theme: &mut Theme, settings: &Settings) {
    let is_dark = theme.is_dark();
    let Some(primary) = resolve_accent_color(settings, is_dark) else {
        return;
    };
    let fg = readable_on(primary);
    let hover = if is_dark {
        primary.lighten(0.12)
    } else {
        primary.darken(0.08)
    };
    let active = primary.darken(if is_dark { 0.1 } else { 0.14 });

    theme.primary = primary;
    theme.primary_foreground = fg;
    theme.primary_hover = hover;
    theme.primary_active = active;
    theme.progress_bar = primary;
    theme.ring = primary;
    theme.link = primary;
    theme.link_hover = hover;
    theme.link_active = active;
    theme.selection = with_alpha(primary, 0.28);
    theme.list_active = with_alpha(primary, if is_dark { 0.18 } else { 0.12 });
    theme.list_active_border = with_alpha(primary, 0.55);
    theme.table_active = with_alpha(primary, if is_dark { 0.18 } else { 0.12 });
    theme.table_active_border = with_alpha(primary, 0.55);
    theme.sidebar_primary = primary;
    theme.sidebar_primary_foreground = fg;
    theme.sidebar_accent = with_alpha(primary, if is_dark { 0.22 } else { 0.12 });
    theme.sidebar_accent_foreground = if is_dark {
        hsla(0.0, 0.0, 0.96, 1.0)
    } else {
        primary.darken(0.25)
    };
    theme.slider_bar = primary;
    theme.slider_thumb = primary;
    theme.drag_border = with_alpha(primary, 0.65);
    theme.drop_target = with_alpha(primary, 0.2);
    theme.accent = if is_dark {
        with_alpha(primary.mix(theme.background, 0.35), 1.0).lightness(0.22)
    } else {
        with_alpha(primary.mix(theme.background, 0.15), 1.0).lightness(0.94)
    };
    theme.accent_foreground = if is_dark {
        hsla(0.0, 0.0, 0.96, 1.0)
    } else {
        primary.darken(0.3)
    };
}

/// Soften large surface tokens for per-pixel translucency (non-Windows / composition path).
#[cfg(not(windows))]
fn apply_surface_opacity(theme: &mut Theme, alpha: f32) {
    if (alpha - 1.0).abs() < 0.001 {
        return;
    }
    let elevated = (alpha + 0.08).min(1.0);

    theme.background = theme.background.divide(alpha);
    theme.sidebar = theme.sidebar.divide(alpha);
    theme.title_bar = theme.title_bar.divide(alpha);
    theme.list = theme.list.divide(alpha);
    theme.list_even = theme.list_even.divide(alpha);
    theme.list_head = theme.list_head.divide(elevated);
    theme.table = theme.table.divide(alpha);
    theme.table_even = theme.table_even.divide(alpha);
    theme.table_head = theme.table_head.divide(elevated);
    theme.popover = theme.popover.divide(elevated);
    theme.group_box = theme.group_box.divide(elevated);
    theme.secondary = theme.secondary.divide(elevated);
    theme.muted = theme.muted.divide(elevated);
    theme.tab_bar = theme.tab_bar.divide(alpha);
    theme.tab = theme.tab.divide(alpha);
    theme.accordion = theme.accordion.divide(alpha);
}

fn apply_shape_and_density(theme: &mut Theme, settings: &Settings) {
    let (r, r_lg) = settings.corner_radius.radii();
    theme.radius = px(r);
    theme.radius_lg = px(r_lg);
    theme.font_size = px(settings.ui_density.font_size());
    // Slightly stronger borders when glassed for readability.
    if settings.window_transparency > 0 {
        theme.border = theme.border.darken(0.08);
        theme.sidebar_border = theme.sidebar_border.darken(0.08);
    }
}

/// Apply whole-window transparency (+ optional backdrop blur) on Windows.
///
/// Layered alpha multiplies the final frame (reliable translucency). When blur is
/// requested and the window is not solid, also enable DWM acrylic-style blur.
#[cfg(windows)]
pub fn apply_window_opacity(window: &Window, transparency_pct: u8, backdrop_blur: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongW, SetLayeredWindowAttributes, SetWindowLongW, GWL_EXSTYLE, LWA_ALPHA,
        WS_EX_LAYERED,
    };

    let Ok(handle) = <Window as HasWindowHandle>::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };

    let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
    let alpha = (effective_window_opacity_alpha(transparency_pct) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;

    unsafe {
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
        if alpha == 255 {
            let cleared = (ex as u32) & !WS_EX_LAYERED.0;
            SetWindowLongW(hwnd, GWL_EXSTYLE, cleared as i32);
            window.set_background_appearance(WindowBackgroundAppearance::Opaque);
        } else {
            let layered = (ex as u32) | WS_EX_LAYERED.0;
            SetWindowLongW(hwnd, GWL_EXSTYLE, layered as i32);
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
            if backdrop_blur {
                window.set_background_appearance(WindowBackgroundAppearance::Blurred);
            } else {
                window.set_background_appearance(WindowBackgroundAppearance::Transparent);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn apply_window_opacity(window: &Window, transparency_pct: u8, backdrop_blur: bool) {
    let alpha = effective_window_opacity_alpha(transparency_pct);
    if alpha >= 0.999 {
        window.set_background_appearance(WindowBackgroundAppearance::Opaque);
    } else if backdrop_blur {
        window.set_background_appearance(WindowBackgroundAppearance::Blurred);
    } else {
        window.set_background_appearance(WindowBackgroundAppearance::Transparent);
    }
}

/// Full appearance apply: theme → accent → density/radius → chrome.
pub fn apply_appearance(settings: &Settings, window: Option<&mut Window>, cx: &mut App) {
    let mut settings = settings.clone();
    settings.sanitize_appearance();

    let mode = resolve_theme_mode(settings.theme, window.as_deref(), cx);
    Theme::change(mode, None, cx);

    {
        let theme = Theme::global_mut(cx);
        apply_accent(theme, &settings);
        apply_shape_and_density(theme, &settings);
        // On Windows, layered alpha handles translucency — keep surfaces solid.
        // Elsewhere, fade theme surfaces for per-pixel glass.
        #[cfg(not(windows))]
        {
            let effective = effective_window_opacity_alpha(settings.window_transparency);
            apply_surface_opacity(theme, effective);
        }
    }

    if let Some(window) = window {
        apply_window_opacity(window, settings.window_transparency, settings.backdrop_blur);
        window.refresh();
    }
}

/// Whether the noise overlay should paint at this intensity.
pub fn noise_enabled(intensity: u8) -> bool {
    intensity > 0
}

/// Whether the vignette overlay should paint.
pub fn vignette_enabled(intensity: u8) -> bool {
    intensity > 0
}

/// Edge vignette alpha from intensity 0..100 (peak ~0.42 at the rim).
pub fn vignette_edge_alpha(intensity: u8) -> f32 {
    let t = (intensity.min(MAX_VIGNETTE_INTENSITY) as f32) / 100.0;
    t * 0.42
}

/// Quantize slider 1–100 into cache keys so we don't rebuild every drag step.
pub fn noise_cache_key(intensity: u8) -> u8 {
    let i = intensity.min(MAX_NOISE_INTENSITY);
    if i == 0 {
        return 0;
    }
    // 5% steps: 1–5 → 5, 6–10 → 10, …, 96–100 → 100
    (((i as u16 + 4) / 5) * 5).min(100) as u8
}

/// Film-grain strength baked into the texture (0.0–1.0).
///
/// Canvas `.opacity()` is a no-op in GPUI's `Style::paint`, so intensity must
/// live in per-pixel alpha — not element opacity.
///
/// Slider **100%** matches the former **~25%** strength (anything above that
/// was too heavy).
fn noise_strength(intensity: u8) -> f32 {
    let t = noise_cache_key(intensity) as f32 / 100.0;
    // Map full slider range onto the old 0–25% band.
    let former_t = t * 0.25;
    (former_t.powf(0.85) * 0.78).clamp(0.0, 1.0)
}

/// Cached grain texture for a given slider intensity (0 → empty / unused).
pub fn film_grain_image(intensity: u8) -> Arc<RenderImage> {
    use std::sync::Mutex;
    type Cache = std::collections::HashMap<u8, Arc<RenderImage>>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

    let key = noise_cache_key(intensity);
    let cache = CACHE.get_or_init(|| Mutex::new(Cache::new()));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .entry(key)
        .or_insert_with(|| Arc::new(build_film_grain_texture(512, noise_strength(key))))
        .clone()
}

/// Dense dual-polarity film grain (BGRA for [`RenderImage`]).
///
/// - Continuous coverage (not sparse "stars")
/// - Black + white flecks with low–medium alpha (not mid-gray fog)
/// - `strength` (0–1) scales fleck alpha so 1% ≠ 100%
fn build_film_grain_texture(size: u32, strength: f32) -> RenderImage {
    let size = size.max(16);
    let strength = strength.clamp(0.0, 1.0);
    let mut state: u64 = 0xF11C_A1B2_C3D4_E5F6 ^ 0x00C0_FFEE_4242;
    let mut next_u01 = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state & 0xFFFF) as f32 / 65535.0
    };

    // High-frequency field in [-1, 1].
    let mut field = vec![0.0f32; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let fine = next_u01() * 2.0 - 1.0;
            let mid = next_u01() * 2.0 - 1.0;
            field[(y * size + x) as usize] = fine * 0.78 + mid * 0.22;
        }
    }

    // Very light cross soften — keeps density, softens pure 1px sparkles.
    let mut soft = vec![0.0f32; field.len()];
    for y in 0..size as i32 {
        for x in 0..size as i32 {
            let c = field[(y as u32 * size + x as u32) as usize];
            let mut acc = c * 5.0;
            let mut wsum = 5.0;
            for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                let xx = (x + dx).rem_euclid(size as i32) as u32;
                let yy = (y + dy).rem_euclid(size as i32) as u32;
                acc += field[(yy * size + xx) as usize];
                wsum += 1.0;
            }
            soft[(y as u32 * size + x as u32) as usize] = acc / wsum;
        }
    }

    // Peak fleck alpha at full strength — enough grit, not a fog.
    let max_a = 28.0 + strength * 72.0; // ~28 at tiny, ~100 at full

    let mut img = RgbaImage::new(size, size);
    for (i, pixel) in img.pixels_mut().enumerate() {
        let n = soft[i].clamp(-1.0, 1.0);
        // No large dead-zone: continuous grain (stars came from high thresholds).
        // pow < 1 boosts mid values so the field looks filled, not dotted.
        let mag = n.abs().powf(0.62);
        let a = (mag * max_a * strength).round().clamp(0.0, 255.0) as u8;
        if a < 2 {
            *pixel = Rgba([0, 0, 0, 0]);
            continue;
        }
        if n >= 0.0 {
            *pixel = Rgba([255, 255, 255, a]);
        } else {
            *pixel = Rgba([0, 0, 0, a]);
        }
    }

    // RGBA → BGRA for GPUI.
    for px in img.chunks_exact_mut(4) {
        px.swap(0, 2);
    }

    let frame = Frame::new(img);
    let mut frames: SmallVec<[Frame; 1]> = SmallVec::new();
    frames.push(frame);
    RenderImage::new(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_accent_is_none() {
        let s = Settings::default();
        assert!(resolve_accent_color(&s, true).is_none());
    }

    #[test]
    fn custom_accent_uses_hue() {
        let mut s = Settings::default();
        s.accent_preset = AccentPreset::Custom;
        s.accent_hue = 120.0;
        s.accent_saturation = 70.0;
        s.accent_lightness = 48.0;
        let c = resolve_accent_color(&s, false).unwrap();
        assert!((c.h - 120.0 / 360.0).abs() < 0.02);
    }

    #[test]
    fn custom_accent_respects_lightness() {
        let mut s = Settings::default();
        s.accent_preset = AccentPreset::Custom;
        s.accent_hue = 200.0;
        s.accent_saturation = 80.0;
        s.accent_lightness = 30.0;
        let dark = resolve_accent_color(&s, true).unwrap();
        s.accent_lightness = 70.0;
        let light = resolve_accent_color(&s, true).unwrap();
        assert!(light.l > dark.l);
    }

    #[test]
    fn transparency_slider_maps_with_floor() {
        // 0% = solid (default)
        assert!((effective_window_opacity_alpha(0) - 1.0).abs() < 0.001);
        // 100% = max glass, still floored
        assert!((effective_window_opacity_alpha(100) - 0.75).abs() < 0.001);
        let mid = effective_window_opacity_alpha(50);
        assert!(mid > 0.75 && mid < 1.0);
        assert!((mid - 0.875).abs() < 0.001);
    }

    #[test]
    fn vignette_scales_with_intensity() {
        assert!(!vignette_enabled(0));
        assert!(vignette_enabled(1));
        assert_eq!(vignette_edge_alpha(0), 0.0);
        assert!((vignette_edge_alpha(100) - 0.42).abs() < 0.001);
        assert!(vignette_edge_alpha(50) < vignette_edge_alpha(100));
    }

    #[test]
    fn noise_strength_ramps_with_slider() {
        assert!(!noise_enabled(0));
        assert!(noise_enabled(1));
        assert_eq!(noise_cache_key(0), 0);
        assert_eq!(noise_cache_key(3), 5);
        assert_eq!(noise_cache_key(100), 100);
        let s5 = noise_strength(5);
        let s50 = noise_strength(50);
        let s100 = noise_strength(100);
        assert!(s5 < s50 && s50 < s100, "s5={s5} s50={s50} s100={s100}");
        // 100% is capped at the old ~25% band (not the old full 0.78 peak).
        let old_25 = (0.25f32).powf(0.85) * 0.78;
        assert!((s100 - old_25).abs() < 0.02, "s100={s100} old_25={old_25}");
    }

    #[test]
    fn grain_texture_is_dense_not_stars() {
        let low = film_grain_image(10);
        let high = film_grain_image(100);
        assert!(low.frame_count() >= 1 && high.frame_count() >= 1);

        let mean_a = |img: &RenderImage| {
            let bytes = img.as_bytes(0).unwrap();
            let mut sum = 0u64;
            let mut n = 0u64;
            for px in bytes.chunks_exact(4) {
                sum += px[3] as u64;
                n += 1;
            }
            sum as f32 / n as f32
        };

        let a_low = mean_a(&low);
        let a_high = mean_a(&high);
        // High intensity must bake stronger alpha than low (opacity path is broken).
        assert!(
            a_high > a_low * 1.8,
            "expected intensity baked into alpha: low={a_low} high={a_high}"
        );

        // Dense field: a large share of pixels carry some fleck (not starfield).
        let bytes = high.as_bytes(0).unwrap();
        let flecks = bytes.chunks_exact(4).filter(|px| px[3] >= 4).count();
        let ratio = flecks as f32 / (bytes.len() / 4) as f32;
        assert!(
            ratio > 0.35,
            "expected dense grain coverage, fleck_ratio={ratio}"
        );
    }
}

/// CSS-friendly appearance DTO for the browser extension popup/options.
pub fn appearance_settings_dto(settings: &Settings) -> serde_json::Value {
    let theme = match settings.theme {
        AppTheme::Light => "light",
        AppTheme::Dark => "dark",
        AppTheme::System => "system",
    };
    serde_json::json!({
        "theme": theme,
        "accentColor": accent_to_hex(settings),
    })
}

/// Resolve the active accent to `#RRGGBB` for extension theming.
pub fn accent_to_hex(settings: &Settings) -> String {
    let color = resolve_accent_color(settings, true)
        .unwrap_or_else(|| gpui_component::ColorName::Blue.scale(400));
    hsla_to_hex(color)
}

fn hsla_to_hex(color: Hsla) -> String {
    // gpui Hsla: h in 0..1 turns, s/l in 0..1
    let h = color.h * 360.0;
    let s = color.s;
    let l = color.l;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = if (0.0..60.0).contains(&h) {
        (c, x, 0.0)
    } else if (60.0..120.0).contains(&h) {
        (x, c, 0.0)
    } else if (120.0..180.0).contains(&h) {
        (0.0, c, x)
    } else if (180.0..240.0).contains(&h) {
        (0.0, x, c)
    } else if (240.0..300.0).contains(&h) {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let r = ((r1 + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    let g = ((g1 + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    let b = ((b1 + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("#{r:02x}{g:02x}{b:02x}")
}
