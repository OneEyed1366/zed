use gpui::{AnyElement, App, Context, Hsla, Rgba, ScrollHandle, SharedString, Window};
use settings::{Settings as _, ThemeColorsContent, ThemeStyleContent, WindowBackgroundContent};
use theme::{ActiveTheme, ThemeColors, ThemeRegistry};
use ui::{ContextMenu, DropdownMenu, DropdownStyle, IconButton, IconName, IconPosition, prelude::*};
use util::ResultExt as _;

use crate::components::{SettingsInputField, SettingsSectionHeader};
use crate::{SettingsUiFile, SettingsWindow, update_settings_file};

const THEME_JSON_FIELD: &str = "theme.theme_overrides";

fn percent_to_alpha_byte(percent: u8) -> u8 {
    ((percent.min(100) as f32 / 100.0) * 255.0).round() as u8
}

fn alpha_byte_to_percent(alpha: u8) -> u8 {
    ((alpha as f32 / 255.0) * 100.0).round() as u8
}

/// Blends `color`'s RGB onto `base` using `color`'s own native alpha — the
/// same math the compositor performs when painting `color` over `base`.
/// Needed because some theme keys are defined with alpha 0 (a "let whatever
/// is behind this show through" placeholder, paired with an arbitrary RGB —
/// often black) so a theme can look fully transparent in that spot under
/// `background.appearance: transparent/blurred`. Reusing that RGB directly
/// at a NEW, higher alpha would paint a solid black rectangle instead of a
/// tinted version of what the theme actually shows there; blending against
/// the theme's own opaque `background` first recovers a meaningful color.
fn blend_onto(base: Hsla, color: Hsla) -> Rgba {
    let base: Rgba = base.into();
    let color: Rgba = color.into();
    let a = color.a;
    Rgba {
        r: base.r * (1.0 - a) + color.r * a,
        g: base.g * (1.0 - a) + color.g * a,
        b: base.b * (1.0 - a) + color.b * a,
        a: 1.0,
    }
}

/// Serializes an RGB color with a REPLACED alpha (0-100 percent). Alpha
/// always comes from the user's current field value — never from a
/// previously-applied override, so repeated edits never compound.
fn rgba_hex_with_alpha_percent(rgba: Rgba, alpha_percent: u8) -> String {
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        (rgba.r * 255.0).round() as u8,
        (rgba.g * 255.0).round() as u8,
        (rgba.b * 255.0).round() as u8,
        percent_to_alpha_byte(alpha_percent),
    )
}

fn effective_alpha_percent(color: Hsla) -> u8 {
    alpha_byte_to_percent((color.a * 255.0).round() as u8)
}

struct ColorKey {
    native: fn(&ThemeColors) -> Hsla,
    override_get: fn(&ThemeColorsContent) -> Option<&String>,
    override_set: fn(&mut ThemeColorsContent, Option<String>),
}

struct ColorSection {
    title: &'static str,
    keys: &'static [ColorKey],
    /// Whether the global Opacity field broadcasts to this section. Popups &
    /// overlays (tooltips, LSP hover cards, palettes, notifications, modals —
    /// all backed by `elevated_surface_background`, see
    /// `ui::styles::elevation::ElevationIndex::bg`) are excluded because text
    /// on top of them needs to stay legible even when the user dials the
    /// rest of the window down. Title Bar and Status/Tab Bar are excluded
    /// too, but for a different reason: they get their own fixed default
    /// (see `DEFAULTED_ON_NON_OPAQUE` in `set_blur`) instead of tracking
    /// whatever the user picks for the editor/panels. Excluded sections are
    /// only reachable through their own explicit Advanced field.
    included_in_global: bool,
}

macro_rules! color_key {
    ($field:ident) => {
        ColorKey {
            native: |c| c.$field,
            override_get: |c| c.$field.as_ref(),
            override_set: |c, v| c.$field = v,
        }
    };
}

const SURFACES_SECTION_TITLE: &str = "Surfaces";
const TITLE_BAR_SECTION_TITLE: &str = "Title Bar";
const STATUS_TAB_BAR_SECTION_TITLE: &str = "Status/Tab Bar";

static SECTIONS: &[ColorSection] = &[
    ColorSection {
        title: TITLE_BAR_SECTION_TITLE,
        keys: &[
            color_key!(title_bar_background),
            color_key!(title_bar_inactive_background),
        ],
        included_in_global: false,
    },
    ColorSection {
        title: "Panels",
        keys: &[color_key!(panel_background)],
        included_in_global: true,
    },
    ColorSection {
        title: "Editor",
        // `toolbar_background` also appears in Surfaces below: it paints the
        // breadcrumb strip directly above the buffer (`workspace::toolbar`),
        // which reads as part of "the editor" even though the same color is
        // shared by the search bar, agent panel, and git graph toolbars.
        // Listing it in both sections means whichever you edit last wins —
        // same "last write wins" rule as Global vs a single section.
        keys: &[
            color_key!(editor_background),
            color_key!(editor_gutter_background),
            color_key!(toolbar_background),
        ],
        included_in_global: true,
    },
    ColorSection {
        title: "Terminal",
        // `terminal_ansi_background` is the actual cell-fill color the PTY
        // grid renders with (`NamedColor::Background`, see
        // `terminal_view::terminal_element` — every idle cell and any
        // full-screen TUI app that clears to "default background" resolves
        // through this key, not `terminal_background`). It's a dedicated
        // background slot, distinct from the 16 named ANSI text colors, so
        // including it here doesn't fade actual terminal text.
        keys: &[
            color_key!(terminal_background),
            color_key!(terminal_ansi_background),
        ],
        included_in_global: true,
    },
    ColorSection {
        title: STATUS_TAB_BAR_SECTION_TITLE,
        keys: &[
            color_key!(status_bar_background),
            color_key!(tab_bar_background),
            color_key!(tab_inactive_background),
        ],
        included_in_global: false,
    },
    ColorSection {
        title: SURFACES_SECTION_TITLE,
        keys: &[
            color_key!(background),
            color_key!(surface_background),
            color_key!(toolbar_background),
        ],
        included_in_global: true,
    },
    ColorSection {
        title: "Popups & Overlays",
        keys: &[color_key!(elevated_surface_background)],
        included_in_global: false,
    },
];

fn active_theme_name(cx: &App) -> SharedString {
    cx.theme().name.clone()
}

/// The theme's own defined colors, ignoring any currently-applied override.
fn native_colors(cx: &App, theme_name: &str) -> ThemeColors {
    ThemeRegistry::global(cx)
        .get(theme_name)
        .map(|theme| theme.colors().clone())
        .unwrap_or_else(|_| cx.theme().colors().clone())
}

fn current_override(cx: &App, theme_name: &str) -> Option<ThemeStyleContent> {
    theme_settings::ThemeSettings::get_global(cx)
        .theme_overrides
        .get(theme_name)
        .cloned()
}

fn set_section_alpha(
    theme_name: SharedString,
    section: &'static ColorSection,
    alpha_percent: u8,
    window: &mut Window,
    cx: &mut App,
) {
    let native = native_colors(cx, &theme_name);
    let theme_name = theme_name.to_string();
    update_settings_file(SettingsUiFile::User, Some(THEME_JSON_FIELD), window, cx, move |settings, _app| {
        let entry = settings.theme.theme_overrides.entry(theme_name).or_default();
        for key in section.keys {
            let effective = blend_onto(native.background, (key.native)(&native));
            let hex = rgba_hex_with_alpha_percent(effective, alpha_percent);
            (key.override_set)(&mut entry.colors, Some(hex));
        }
    })
    .log_err();
}

/// Global unconditionally broadcasts to every `included_in_global` section —
/// it's a "set everything to X" action, not a "set everything that hasn't
/// been touched" action. A prior per-section edit is a completely separate
/// write into the same override entry; there is no persisted "detached"
/// state to track, so global re-touching a section it already broadcast to
/// previously is not a special case, it's just the same write happening
/// again. Sections with `included_in_global: false` (Popups & Overlays) are
/// never touched here — only their own Advanced field can change them.
fn set_global_alpha(theme_name: SharedString, alpha_percent: u8, window: &mut Window, cx: &mut App) {
    for section in SECTIONS.iter().filter(|s| s.included_in_global) {
        set_section_alpha(theme_name.clone(), section, alpha_percent, window, cx);
    }
}

/// Clears just this section's override keys, leaving the rest of the
/// theme's override entry (syntax, players, an unrelated hand-edited color,
/// etc.) untouched.
fn reset_section(theme_name: SharedString, section: &'static ColorSection, window: &mut Window, cx: &mut App) {
    let theme_name = theme_name.to_string();
    update_settings_file(SettingsUiFile::User, Some(THEME_JSON_FIELD), window, cx, move |settings, _app| {
        if let Some(entry) = settings.theme.theme_overrides.get_mut(&theme_name) {
            for key in section.keys {
                (key.override_set)(&mut entry.colors, None);
            }
            if *entry == ThemeStyleContent::default() {
                settings.theme.theme_overrides.remove(&theme_name);
            }
        }
    })
    .log_err();
}

/// Applied to `DEFAULTED_ON_NON_OPAQUE` sections the first time the window
/// leaves `Opaque`, so title bar and tabs read as translucent immediately
/// instead of staying at the theme's native (usually solid) color until the
/// user finds them in Advanced.
const DEFAULT_NON_OPAQUE_PERCENT: u8 = 85;

/// Sections excluded from the global field (see `ColorSection::included_in_global`)
/// that get `DEFAULT_NON_OPAQUE_PERCENT` instead, so they aren't left fully
/// opaque with no way to discover them short of opening Advanced.
const DEFAULTED_ON_NON_OPAQUE: &[&str] = &[TITLE_BAR_SECTION_TITLE, STATUS_TAB_BAR_SECTION_TITLE];

fn set_blur(theme_name: SharedString, value: WindowBackgroundContent, window: &mut Window, cx: &mut App) {
    let existing = current_override(cx, &theme_name);

    let theme_name_key = theme_name.to_string();
    update_settings_file(SettingsUiFile::User, Some(THEME_JSON_FIELD), window, cx, move |settings, _app| {
        settings
            .theme
            .theme_overrides
            .entry(theme_name_key)
            .or_default()
            .window_background_appearance = Some(value);
    })
    .log_err();

    if value == WindowBackgroundContent::Opaque {
        return;
    }
    for section in SECTIONS.iter().filter(|s| DEFAULTED_ON_NON_OPAQUE.contains(&s.title)) {
        let already_set = existing
            .as_ref()
            .is_some_and(|o| (section.keys[0].override_get)(&o.colors).is_some());
        if !already_set {
            set_section_alpha(theme_name.clone(), section, DEFAULT_NON_OPAQUE_PERCENT, window, cx);
        }
    }
}

fn section_percent(native: &ThemeColors, override_content: Option<&ThemeStyleContent>, section: &ColorSection) -> u8 {
    override_content
        .and_then(|o| (section.keys[0].override_get)(&o.colors))
        .and_then(|hex| Rgba::try_from(hex.as_str()).ok())
        .map(|rgba| alpha_byte_to_percent((rgba.a * 255.0).round() as u8))
        .unwrap_or_else(|| effective_alpha_percent((section.keys[0].native)(native)))
}

fn render_blur_dropdown(
    current: WindowBackgroundContent,
    theme_name: SharedString,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    const OPTIONS: [(WindowBackgroundContent, &str); 3] = [
        (WindowBackgroundContent::Opaque, "Opaque"),
        (WindowBackgroundContent::Transparent, "Transparent"),
        (WindowBackgroundContent::Blurred, "Blurred"),
    ];
    let current_label = OPTIONS
        .iter()
        .find(|(value, _)| *value == current)
        .map(|(_, label)| *label)
        .unwrap_or("Opaque");

    let menu = ContextMenu::build(window, cx, {
        move |mut menu, _, _cx| {
            for (value, label) in OPTIONS {
                let theme_name = theme_name.clone();
                menu = menu.toggleable_entry(label, value == current, IconPosition::End, None, move |window, cx| {
                    set_blur(theme_name.clone(), value, window, cx);
                });
            }
            menu
        }
    });

    DropdownMenu::new("transparency-blur-dropdown", current_label, menu)
        .style(DropdownStyle::Outlined)
        .into_any_element()
}

fn render_global_opacity_field(percent: u8, theme_name: SharedString) -> AnyElement {
    render_labeled_row(
        "Opacity",
        SettingsInputField::new("transparency-global-opacity")
            .with_initial_text(percent.to_string())
            .display_confirm_button()
            .on_confirm(move |value, window, cx| {
                let Some(value) = value else {
                    return;
                };
                if let Ok(percent) = value.trim().parse::<u8>() {
                    set_global_alpha(theme_name.clone(), percent.min(100), window, cx);
                }
            })
            .into_any_element(),
    )
}

/// A settings row shaped like the rest of the Settings UI: label on the
/// left, a fixed-width control on the right. Without this, a bare
/// `SettingsInputField` stretches to the full width of its `v_flex` parent
/// instead of sitting in a compact `min_w_64` box.
fn render_labeled_row(label: &'static str, control: AnyElement) -> AnyElement {
    h_flex()
        .justify_between()
        .gap_2()
        .child(Label::new(label))
        .child(control)
        .into_any_element()
}

fn render_advanced_link(cx: &mut Context<SettingsWindow>) -> AnyElement {
    ui::Button::new("open-advanced-transparency", "Advanced (per-section)")
        .on_click(cx.listener(|this, _, window, cx| {
            this.push_dynamic_sub_page(
                "Advanced Transparency",
                "Transparency & Blur",
                None,
                false,
                render_transparency_blur_advanced_page,
                window,
                cx,
            );
        }))
        .into_any_element()
}

pub(crate) fn render_transparency_blur_page(
    _settings_window: &SettingsWindow,
    scroll_handle: &ScrollHandle,
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let theme_name = active_theme_name(cx);
    let native = native_colors(cx, &theme_name);
    let override_content = current_override(cx, &theme_name);

    let current_blur = override_content
        .as_ref()
        .and_then(|o| o.window_background_appearance)
        .unwrap_or(WindowBackgroundContent::Opaque);

    // Falls back to the theme's native `background` alpha if SECTIONS were
    // ever restructured without a Surfaces entry — avoids panicking over a
    // display-only reference value.
    let global_percent = SECTIONS
        .iter()
        .find(|s| s.title == SURFACES_SECTION_TITLE)
        .map(|section| section_percent(&native, override_content.as_ref(), section))
        .unwrap_or_else(|| effective_alpha_percent(native.background));

    let show_opacity_controls = current_blur != WindowBackgroundContent::Opaque;
    let blur_dropdown = render_blur_dropdown(current_blur, theme_name.clone(), window, cx);

    v_flex()
        .id("transparency-blur-page")
        .size_full()
        .pt_2p5()
        .px_8()
        .pb_16()
        .gap_4()
        .overflow_y_scroll()
        .track_scroll(scroll_handle)
        .child(SettingsSectionHeader::new("Transparency & Blur").no_padding(true))
        .child(render_labeled_row("Blur", blur_dropdown))
        .when(show_opacity_controls, |this| {
            this.child(render_global_opacity_field(global_percent, theme_name.clone()))
                .child(render_advanced_link(cx))
        })
        .into_any_element()
}

fn render_section_row(section: &'static ColorSection, native: &ThemeColors, override_content: Option<&ThemeStyleContent>, theme_name: SharedString) -> AnyElement {
    let percent = section_percent(native, override_content, section);

    h_flex()
        .justify_between()
        .gap_2()
        .child(Label::new(section.title))
        .child(
            h_flex()
                .flex_none()
                .gap_2()
                .child(
                    SettingsInputField::new(format!("transparency-section-{}", section.title))
                        .with_initial_text(percent.to_string())
                        .display_confirm_button()
                        .on_confirm({
                            let theme_name = theme_name.clone();
                            move |value, window, cx| {
                                let Some(value) = value else {
                                    return;
                                };
                                if let Ok(percent) = value.trim().parse::<u8>() {
                                    set_section_alpha(theme_name.clone(), section, percent.min(100), window, cx);
                                }
                            }
                        }),
                )
                .child(
                    IconButton::new(format!("reset-transparency-section-{}", section.title), IconName::Undo).on_click(
                        move |_, window, cx| {
                            reset_section(theme_name.clone(), section, window, cx);
                        },
                    ),
                ),
        )
        .into_any_element()
}

pub(crate) fn render_transparency_blur_advanced_page(
    _settings_window: &SettingsWindow,
    scroll_handle: &ScrollHandle,
    _window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let theme_name = active_theme_name(cx);
    let native = native_colors(cx, &theme_name);
    let override_content = current_override(cx, &theme_name);

    v_flex()
        .id("transparency-blur-advanced-page")
        .size_full()
        .pt_2p5()
        .px_8()
        .pb_16()
        .gap_4()
        .overflow_y_scroll()
        .track_scroll(scroll_handle)
        .child(SettingsSectionHeader::new("Advanced Transparency").no_padding(true))
        .children(
            SECTIONS
                .iter()
                .map(|section| render_section_row(section, &native, override_content.as_ref(), theme_name.clone())),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::hsla;

    #[test]
    fn percent_to_alpha_byte_roundtrip() {
        assert_eq!(percent_to_alpha_byte(100), 255);
        assert_eq!(percent_to_alpha_byte(0), 0);
        assert_eq!(percent_to_alpha_byte(50), 128);
    }

    #[test]
    fn alpha_byte_to_percent_roundtrip() {
        assert_eq!(alpha_byte_to_percent(255), 100);
        assert_eq!(alpha_byte_to_percent(0), 0);
        assert_eq!(alpha_byte_to_percent(128), 50);
    }

    #[test]
    fn rgba_hex_with_alpha_percent_only_changes_alpha() {
        let rgba: Rgba = hsla(0.5, 0.5, 0.5, 1.0).into();
        let hex_full = rgba_hex_with_alpha_percent(rgba, 100);
        assert!(hex_full.ends_with("ff"), "100% alpha must serialize fully opaque: {hex_full}");

        let hex_half = rgba_hex_with_alpha_percent(rgba, 50);
        assert_eq!(&hex_half[0..7], &hex_full[0..7], "RGB must be unchanged");
        assert_eq!(&hex_half[7..9], "80", "alpha byte must be 50% = 0x80");
    }

    #[test]
    fn blend_onto_recovers_base_when_color_is_fully_transparent() {
        // Mirrors a theme key like `terminal.background: #00000000` — alpha
        // 0 with an arbitrary (often black) RGB. Naively reusing that RGB
        // at a new nonzero alpha would paint solid black; blending must
        // recover the theme's own `background` color instead.
        let base = hsla(0.1, 0.3, 0.2, 1.0);
        let invisible_placeholder = hsla(0.0, 0.0, 0.0, 0.0);

        let blended = blend_onto(base, invisible_placeholder);
        let base_rgba: Rgba = base.into();
        assert!((blended.r - base_rgba.r).abs() < 0.001);
        assert!((blended.g - base_rgba.g).abs() < 0.001);
        assert!((blended.b - base_rgba.b).abs() < 0.001);
    }

    #[test]
    fn blend_onto_keeps_color_when_fully_opaque() {
        let base = hsla(0.1, 0.3, 0.2, 1.0);
        let opaque_color = hsla(0.6, 0.5, 0.4, 1.0);

        let blended = blend_onto(base, opaque_color);
        let color_rgba: Rgba = opaque_color.into();
        assert!((blended.r - color_rgba.r).abs() < 0.001);
        assert!((blended.g - color_rgba.g).abs() < 0.001);
        assert!((blended.b - color_rgba.b).abs() < 0.001);
    }

    #[test]
    fn effective_alpha_percent_reads_hsla_alpha_directly() {
        assert_eq!(effective_alpha_percent(hsla(0.0, 0.0, 0.0, 0.25)), 25);
        assert_eq!(effective_alpha_percent(hsla(0.0, 0.0, 0.0, 1.0)), 100);
    }
}
