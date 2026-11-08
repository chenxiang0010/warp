//! Formatting helpers and small widgets shared by credit/billing surfaces.

#[cfg(feature = "tui")]
use thousands::Separable;
use warp_core::ui::appearance::Appearance;
use warp_core::ui::theme::Fill;
use warpui::Element;
use warpui::elements::{Container, CornerRadius, Empty, Radius, Text};

#[cfg(feature = "tui")]
pub fn format_credits(credits: f64) -> String {
    (credits.round() as i64).separate_with_commas()
}

#[cfg(feature = "tui")]
pub fn format_cost_cents(cents: f64) -> String {
    let cents = cents.round() as i64;
    let dollars = cents / 100;
    let remainder = (cents.abs() % 100) as u8;
    if dollars < 0 {
        format!(
            "-${}.{remainder:02}",
            dollars.unsigned_abs().separate_with_commas()
        )
    } else {
        format!("${}.{remainder:02}", dollars.separate_with_commas())
    }
}

pub fn create_discount_badge(discount: u32, appearance: &Appearance) -> Box<dyn Element> {
    if discount == 0 {
        return Empty::new().finish();
    }

    let theme = appearance.theme();
    let bg_color: Fill = theme.terminal_colors().normal.green.into();

    Container::new(
        Text::new_inline(format!("{discount}% off"), appearance.ui_font_family(), 10.)
            .with_color(theme.main_text_color(bg_color).into())
            .finish(),
    )
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
    .with_background(bg_color)
    .with_uniform_padding(4.)
    .finish()
}
