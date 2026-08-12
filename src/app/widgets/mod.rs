//! Shared UI widget helpers, split by domain.
//!
//! Call sites continue to import via `super::widgets::{…}` / `widgets::…`.

mod chrome;
mod nav;
mod path;
mod progress;
mod queue;
mod settings;

pub(crate) use chrome::{empty_state_badge, render_vignette_overlay, soft_tooltip};
// format_nav_count is re-exported for path stability (was pub(crate) on the monolith).
#[allow(unused_imports)]
pub(crate) use nav::{format_nav_count, nav_item, settings_nav_item};
pub(crate) use path::{browse_directory, shorten_path_display};
pub(crate) use progress::styled_progress;
pub(crate) use queue::{
    ellipsize_name, file_type_status_tile, metric_cell, name_char_budget, sortable_header,
    status_chip, status_color, status_tag,
};
pub(crate) use settings::{
    accent_custom_swatch, accent_hsl_slider_row, accent_preset_swatch, field_hint, field_label,
    settings_choice_row, settings_field_label, settings_input_with_reset, settings_subgroup,
};
