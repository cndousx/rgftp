//! Animated light/dark theme switching.
//!
//! GPUI can't snapshot the window, so a classic clip-the-new-theme-to-a-circle
//! reveal is out of reach: the palette is a global that every component reads
//! while it renders. Instead the palette itself is interpolated frame by frame
//! and a circular wash expands from the theme button on top of the page, which
//! reads as the new theme spreading out of the control that was clicked.

use std::time::Instant;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::theme::{Colorize as _, Theme, ThemeColor, ThemeMode};
use gpui_kit::{App, Hsla};

/// How long a theme switch takes, in seconds.
pub const DURATION: f32 = 0.42;

/// Linear progress of a transition that started at `started`, clamped to 0..=1.
pub fn progress(started: Instant) -> f32 {
    (started.elapsed().as_secs_f32() / DURATION).clamp(0.0, 1.0)
}

/// Smooth acceleration out of the click point and back to rest at the end.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Mixes two colors in Oklab space, `t` being the weight of `to`.
pub fn mix_color(from: Hsla, to: Hsla, t: f32) -> Hsla {
    from.mix_oklab(to, 1.0 - t)
}

/// The palette a mode resolves to, without switching the app over to it.
pub fn palette_for(mode: ThemeMode, cx: &App) -> ThemeColor {
    let theme = cx.theme();
    let config = if mode.is_dark() {
        theme.dark_theme.clone()
    } else {
        theme.light_theme.clone()
    };
    let mut probe = Theme::default();
    probe.apply_config(&config);
    probe.colors
}

/// Replaces the live palette, keeping every copy of the theme in step.
pub fn apply(colors: ThemeColor, cx: &mut App) {
    Theme::update(cx, move |theme| theme.colors = colors);
}

/// Interpolates every color token so text, borders and backgrounds fade
/// together instead of snapping one by one.
///
/// `ThemeColor` is `#[non_exhaustive]`, so fields added to it in a future
/// release simply keep the starting palette over the transition and snap to
/// their target with the final theme change.
pub fn blend(from: ThemeColor, to: ThemeColor, t: f32) -> ThemeColor {
    let mix = |a: Hsla, b: Hsla| mix_color(a, b, t);
    let mut out = from;
    out.accent = mix(from.accent, to.accent);
    out.accent_foreground = mix(from.accent_foreground, to.accent_foreground);
    out.accordion = mix(from.accordion, to.accordion);
    out.background = mix(from.background, to.background);
    out.border = mix(from.border, to.border);
    out.button = mix(from.button, to.button);
    out.button_active = mix(from.button_active, to.button_active);
    out.button_foreground = mix(from.button_foreground, to.button_foreground);
    out.button_hover = mix(from.button_hover, to.button_hover);
    out.button_danger = mix(from.button_danger, to.button_danger);
    out.button_danger_active = mix(from.button_danger_active, to.button_danger_active);
    out.button_danger_foreground = mix(from.button_danger_foreground, to.button_danger_foreground);
    out.button_danger_hover = mix(from.button_danger_hover, to.button_danger_hover);
    out.button_info = mix(from.button_info, to.button_info);
    out.button_info_active = mix(from.button_info_active, to.button_info_active);
    out.button_info_foreground = mix(from.button_info_foreground, to.button_info_foreground);
    out.button_info_hover = mix(from.button_info_hover, to.button_info_hover);
    out.button_primary = mix(from.button_primary, to.button_primary);
    out.button_primary_active = mix(from.button_primary_active, to.button_primary_active);
    out.button_primary_foreground =
        mix(from.button_primary_foreground, to.button_primary_foreground);
    out.button_primary_hover = mix(from.button_primary_hover, to.button_primary_hover);
    out.button_secondary = mix(from.button_secondary, to.button_secondary);
    out.button_secondary_active = mix(from.button_secondary_active, to.button_secondary_active);
    out.button_secondary_foreground = mix(
        from.button_secondary_foreground,
        to.button_secondary_foreground,
    );
    out.button_secondary_hover = mix(from.button_secondary_hover, to.button_secondary_hover);
    out.button_success = mix(from.button_success, to.button_success);
    out.button_success_active = mix(from.button_success_active, to.button_success_active);
    out.button_success_foreground =
        mix(from.button_success_foreground, to.button_success_foreground);
    out.button_success_hover = mix(from.button_success_hover, to.button_success_hover);
    out.button_warning = mix(from.button_warning, to.button_warning);
    out.button_warning_active = mix(from.button_warning_active, to.button_warning_active);
    out.button_warning_foreground =
        mix(from.button_warning_foreground, to.button_warning_foreground);
    out.button_warning_hover = mix(from.button_warning_hover, to.button_warning_hover);
    out.group_box = mix(from.group_box, to.group_box);
    out.group_box_foreground = mix(from.group_box_foreground, to.group_box_foreground);
    out.caret = mix(from.caret, to.caret);
    out.chart_1 = mix(from.chart_1, to.chart_1);
    out.chart_2 = mix(from.chart_2, to.chart_2);
    out.chart_3 = mix(from.chart_3, to.chart_3);
    out.chart_4 = mix(from.chart_4, to.chart_4);
    out.chart_5 = mix(from.chart_5, to.chart_5);
    out.chart_bullish = mix(from.chart_bullish, to.chart_bullish);
    out.chart_bearish = mix(from.chart_bearish, to.chart_bearish);
    out.chart_grid = mix(from.chart_grid, to.chart_grid);
    out.danger = mix(from.danger, to.danger);
    out.danger_active = mix(from.danger_active, to.danger_active);
    out.danger_foreground = mix(from.danger_foreground, to.danger_foreground);
    out.danger_hover = mix(from.danger_hover, to.danger_hover);
    out.description_list_label = mix(from.description_list_label, to.description_list_label);
    out.description_list_label_foreground = mix(
        from.description_list_label_foreground,
        to.description_list_label_foreground,
    );
    out.drag_border = mix(from.drag_border, to.drag_border);
    out.drop_target = mix(from.drop_target, to.drop_target);
    out.foreground = mix(from.foreground, to.foreground);
    out.info = mix(from.info, to.info);
    out.info_active = mix(from.info_active, to.info_active);
    out.info_foreground = mix(from.info_foreground, to.info_foreground);
    out.info_hover = mix(from.info_hover, to.info_hover);
    out.input = mix(from.input, to.input);
    out.link = mix(from.link, to.link);
    out.link_active = mix(from.link_active, to.link_active);
    out.link_hover = mix(from.link_hover, to.link_hover);
    out.list = mix(from.list, to.list);
    out.list_active = mix(from.list_active, to.list_active);
    out.list_active_border = mix(from.list_active_border, to.list_active_border);
    out.list_even = mix(from.list_even, to.list_even);
    out.list_head = mix(from.list_head, to.list_head);
    out.list_hover = mix(from.list_hover, to.list_hover);
    out.list_highlight_foreground =
        mix(from.list_highlight_foreground, to.list_highlight_foreground);
    out.muted = mix(from.muted, to.muted);
    out.muted_foreground = mix(from.muted_foreground, to.muted_foreground);
    out.popover = mix(from.popover, to.popover);
    out.popover_foreground = mix(from.popover_foreground, to.popover_foreground);
    out.primary = mix(from.primary, to.primary);
    out.primary_active = mix(from.primary_active, to.primary_active);
    out.primary_foreground = mix(from.primary_foreground, to.primary_foreground);
    out.primary_hover = mix(from.primary_hover, to.primary_hover);
    out.progress_bar = mix(from.progress_bar, to.progress_bar);
    out.ring = mix(from.ring, to.ring);
    out.scrollbar = mix(from.scrollbar, to.scrollbar);
    out.scrollbar_thumb = mix(from.scrollbar_thumb, to.scrollbar_thumb);
    out.scrollbar_thumb_hover = mix(from.scrollbar_thumb_hover, to.scrollbar_thumb_hover);
    out.secondary = mix(from.secondary, to.secondary);
    out.secondary_active = mix(from.secondary_active, to.secondary_active);
    out.secondary_foreground = mix(from.secondary_foreground, to.secondary_foreground);
    out.secondary_hover = mix(from.secondary_hover, to.secondary_hover);
    out.selection = mix(from.selection, to.selection);
    out.sidebar = mix(from.sidebar, to.sidebar);
    out.sidebar_accent = mix(from.sidebar_accent, to.sidebar_accent);
    out.sidebar_accent_foreground =
        mix(from.sidebar_accent_foreground, to.sidebar_accent_foreground);
    out.sidebar_border = mix(from.sidebar_border, to.sidebar_border);
    out.sidebar_foreground = mix(from.sidebar_foreground, to.sidebar_foreground);
    out.sidebar_primary = mix(from.sidebar_primary, to.sidebar_primary);
    out.sidebar_primary_foreground = mix(
        from.sidebar_primary_foreground,
        to.sidebar_primary_foreground,
    );
    out.skeleton = mix(from.skeleton, to.skeleton);
    out.slider_bar = mix(from.slider_bar, to.slider_bar);
    out.slider_thumb = mix(from.slider_thumb, to.slider_thumb);
    out.success = mix(from.success, to.success);
    out.success_foreground = mix(from.success_foreground, to.success_foreground);
    out.success_hover = mix(from.success_hover, to.success_hover);
    out.success_active = mix(from.success_active, to.success_active);
    out.switch = mix(from.switch, to.switch);
    out.switch_thumb = mix(from.switch_thumb, to.switch_thumb);
    out.tab = mix(from.tab, to.tab);
    out.tab_active = mix(from.tab_active, to.tab_active);
    out.tab_active_foreground = mix(from.tab_active_foreground, to.tab_active_foreground);
    out.tab_bar = mix(from.tab_bar, to.tab_bar);
    out.tab_bar_segmented = mix(from.tab_bar_segmented, to.tab_bar_segmented);
    out.tab_foreground = mix(from.tab_foreground, to.tab_foreground);
    out.table = mix(from.table, to.table);
    out.table_active = mix(from.table_active, to.table_active);
    out.table_active_border = mix(from.table_active_border, to.table_active_border);
    out.table_even = mix(from.table_even, to.table_even);
    out.table_head = mix(from.table_head, to.table_head);
    out.table_head_foreground = mix(from.table_head_foreground, to.table_head_foreground);
    out.table_foot = mix(from.table_foot, to.table_foot);
    out.table_foot_foreground = mix(from.table_foot_foreground, to.table_foot_foreground);
    out.table_hover = mix(from.table_hover, to.table_hover);
    out.table_row_border = mix(from.table_row_border, to.table_row_border);
    out.title_bar = mix(from.title_bar, to.title_bar);
    out.title_bar_border = mix(from.title_bar_border, to.title_bar_border);
    out.status_bar = mix(from.status_bar, to.status_bar);
    out.status_bar_border = mix(from.status_bar_border, to.status_bar_border);
    out.warning = mix(from.warning, to.warning);
    out.warning_active = mix(from.warning_active, to.warning_active);
    out.warning_hover = mix(from.warning_hover, to.warning_hover);
    out.warning_foreground = mix(from.warning_foreground, to.warning_foreground);
    out.overlay = mix(from.overlay, to.overlay);
    out.window_border = mix(from.window_border, to.window_border);
    out.red = mix(from.red, to.red);
    out.red_light = mix(from.red_light, to.red_light);
    out.green = mix(from.green, to.green);
    out.green_light = mix(from.green_light, to.green_light);
    out.blue = mix(from.blue, to.blue);
    out.blue_light = mix(from.blue_light, to.blue_light);
    out.yellow = mix(from.yellow, to.yellow);
    out.yellow_light = mix(from.yellow_light, to.yellow_light);
    out.magenta = mix(from.magenta, to.magenta);
    out.magenta_light = mix(from.magenta_light, to.magenta_light);
    out.cyan = mix(from.cyan, to.cyan);
    out.cyan_light = mix(from.cyan_light, to.cyan_light);
    out
}
