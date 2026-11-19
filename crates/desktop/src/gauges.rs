//! Gauges: what a session's emulator is doing right now; the bottom strip for the
//! selected session, or each tile's own edge gauges in a split (lotus#70).
//!
//! All rates are guest output per *host* second as measured by the supervisor
//! ([`lotus_session::telemetry`]); none is a guest-tick or hardware-cycle claim.
use std::collections::VecDeque;

use gpui::{
    canvas, div, fill, point, px, size, Bounds, InteractiveElement, IntoElement, ParentElement,
    Rgba, Styled,
};
use lotus_session::telemetry::{Actor, Telemetry, GUEST_REFRESH_HZ};

use crate::layout::{PILL_GAP, PILL_HEIGHT};
use crate::theme;

/// One gauge: label, headline figure and a detail line.
pub struct Gauge {
    pub label: &'static str,
    pub value: String,
    pub colour: Rgba,
    pub detail: String,
}

/// The seven gauges, most important first: the compact strip shows the first
/// [`crate::layout::PRIMARY_GAUGES`] and discloses the rest.
pub fn gauges(theme: theme::Palette, telemetry: &Telemetry) -> [Gauge; 7] {
    let speed_colour = if telemetry.fps == 0.0 {
        theme.text
    } else if telemetry.speed >= 0.97 {
        theme.success
    } else if telemetry.speed >= 0.75 {
        theme.warning
    } else {
        theme.error
    };
    [
        Gauge {
            label: "FPS",
            value: format_rate(telemetry.fps),
            colour: theme.text,
            detail: "guest frames / host second".into(),
        },
        Gauge {
            label: "SPEED",
            value: format_speed(telemetry),
            colour: speed_colour,
            detail: format!("of {GUEST_REFRESH_HZ:.2} Hz"),
        },
        Gauge {
            label: "FRAME",
            value: telemetry.frames.to_string(),
            colour: theme.text,
            detail: format!("epoch {}", telemetry.epoch),
        },
        Gauge {
            label: "AUDIO",
            value: format_audio(telemetry.audio_rate),
            colour: theme.text,
            detail: format!("{} dropped · muted", telemetry.audio_dropped),
        },
        Gauge {
            label: "RENDERER",
            value: if telemetry.renderer.is_empty() {
                "—".to_string()
            } else {
                telemetry.renderer.clone()
            },
            colour: if telemetry.renderer_fallback.is_empty() {
                theme.text
            } else {
                theme.warning
            },
            detail: if telemetry.renderer_fallback.is_empty() {
                telemetry.backend_name.clone()
            } else {
                format!("fallback: {}", telemetry.renderer_fallback)
            },
        },
        Gauge {
            label: "WORKER",
            value: telemetry
                .memory_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "—".into()),
            colour: theme.text,
            detail: telemetry
                .pid
                .map(|pid| format!("pid {pid} · up {}", format_uptime(telemetry.uptime_ms)))
                .unwrap_or_else(|| "not running".into()),
        },
        Gauge {
            label: "CONTROL",
            value: actor_name(telemetry.last_actor).into(),
            colour: theme.text,
            detail: format!(
                "{} · input {}",
                if telemetry.last_operation.is_empty() {
                    "—"
                } else {
                    &telemetry.last_operation
                },
                if telemetry.input_owner.is_empty() {
                    "—"
                } else {
                    &telemetry.input_owner
                }
            ),
        },
    ]
}

/// One row of cards; needs [`crate::layout::CARDS_MIN_WIDTH`].
pub fn gauge_cards(theme: theme::Palette, telemetry: &Telemetry) -> impl IntoElement {
    let mut strip = div()
        .flex()
        .flex_none()
        .gap_2()
        .p_3()
        .border_t_1()
        .border_color(theme.border);
    for (index, gauge) in gauges(theme, telemetry).into_iter().enumerate() {
        let mut card = card(theme, gauge.label);
        if index == 0 {
            card = card.flex_none().w(px(184.0)).child(
                div()
                    .flex()
                    .items_end()
                    .gap_3()
                    .child(figure(theme, gauge.value, gauge.colour))
                    .child(sparkline(theme, &telemetry.fps_history)),
            );
        } else {
            card = card
                .min_w(px(112.0))
                .child(figure(theme, gauge.value, gauge.colour));
        }
        strip = strip.child(card.child(detail(theme, gauge.detail)));
    }
    strip
}

/// One-line label/value pills, detail in a tooltip.
/// `wrap` lets the pills flow onto more rows; without it they stay on one line.
pub fn gauge_pills(
    theme: theme::Palette,
    gauges: impl IntoIterator<Item = Gauge>,
    width: f32,
    wrap: bool,
) -> gpui::Div {
    let mut row = div().flex().flex_none().gap(px(PILL_GAP));
    if wrap {
        row = row.flex_wrap().w_full();
    }
    for gauge in gauges {
        let tooltip = format!(
            "{}: {}
{}",
            gauge.label, gauge.value, gauge.detail
        );
        row = row.child(crate::lotus_window::with_tooltip(
            div()
                .id(gauge.label)
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .flex_none()
                .w(px(width))
                .h(px(PILL_HEIGHT))
                .px_2()
                .rounded(px(5.0))
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.5))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme.text_muted)
                        .child(gauge.label),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.mono)
                        .text_sm()
                        .text_color(gauge.colour)
                        .child(gauge.value),
                ),
            tooltip,
        ));
    }
    row
}

fn card(theme: theme::Palette, label: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .px_3()
        .py_2()
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.border)
        .child(
            div()
                .text_size(px(10.5))
                .text_color(theme.text_muted)
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label),
        )
}

fn figure(theme: theme::Palette, text: String, colour: Rgba) -> impl IntoElement {
    div()
        .font_family(theme.mono)
        .text_size(px(20.0))
        .line_height(px(24.0))
        .text_color(colour)
        .child(text)
}

fn detail(theme: theme::Palette, text: impl Into<gpui::SharedString>) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(theme.text_muted)
        .truncate()
        .child(text.into())
}

/// Bars of recent fps samples, scaled to what was observed so slow sessions
/// still show variation; the full-speed line is drawn only when in range.
fn sparkline(theme: theme::Palette, history: &VecDeque<f32>) -> impl IntoElement {
    let samples: Vec<f32> = history.iter().copied().collect();
    let ceiling = spark_ceiling(&samples);
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let bar = width / crate::gauges::SPARK_BARS as f32;
            if (GUEST_REFRESH_HZ as f32) < ceiling {
                let reference = height * (1.0 - GUEST_REFRESH_HZ as f32 / ceiling);
                window.paint_quad(fill(
                    Bounds::new(
                        bounds.origin + point(px(0.0), px(reference)),
                        size(bounds.size.width, px(1.0)),
                    ),
                    theme.border_strong,
                ));
            }
            let offset = crate::gauges::SPARK_BARS.saturating_sub(samples.len());
            for (index, value) in samples.iter().rev().take(SPARK_BARS).rev().enumerate() {
                let bar_height = (value / ceiling).clamp(0.0, 1.0) * height;
                let x = (offset + index) as f32 * bar;
                window.paint_quad(fill(
                    Bounds::new(
                        bounds.origin + point(px(x), px(height - bar_height)),
                        size(px((bar - 1.0).max(1.0)), px(bar_height.max(1.0))),
                    ),
                    theme.text_secondary,
                ));
            }
        },
    )
    .w(px(96.0))
    .h(px(24.0))
}

/// Bars drawn in a sparkline.
pub const SPARK_BARS: usize = 48;

/// Headroom above the largest sample; snaps to full-speed scale near full speed.
#[must_use]
pub fn spark_ceiling(samples: &[f32]) -> f32 {
    let peak = samples.iter().copied().fold(0.0, f32::max);
    let full = GUEST_REFRESH_HZ as f32;
    if peak >= full * 0.8 {
        (full * 1.25).max(peak * 1.1)
    } else {
        (peak * 1.25).max(1.0)
    }
}

#[must_use]
pub fn format_rate(fps: f64) -> String {
    format!("{fps:.1}")
}

#[must_use]
pub fn format_speed(telemetry: &Telemetry) -> String {
    if telemetry.fps == 0.0 {
        "—".into()
    } else {
        format!("{:.0}%", telemetry.speed * 100.0)
    }
}

#[must_use]
pub fn format_audio(samples_per_second: f64) -> String {
    if samples_per_second == 0.0 {
        "—".into()
    } else {
        format!("{:.1} kHz", samples_per_second / 1000.0)
    }
}

#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    let mib = bytes as f64 / MIB;
    if mib >= 1024.0 {
        format!("{:.2} GB", mib / 1024.0)
    } else {
        format!("{mib:.0} MB")
    }
}

#[must_use]
pub fn format_uptime(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds / 60 % 60),
    }
}

#[must_use]
pub fn actor_name(actor: Actor) -> &'static str {
    match actor {
        Actor::Host => "host",
        Actor::Desktop => "you",
        Actor::Mcp => "agent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn figures_are_compact_and_explicit_when_idle() {
        let mut telemetry = Telemetry::new();
        assert_eq!(format_speed(&telemetry), "—");
        telemetry.fps = GUEST_REFRESH_HZ;
        telemetry.speed = 1.0;
        assert_eq!(format_speed(&telemetry), "100%");
        assert_eq!(format_rate(59.831), "59.8");
        assert_eq!(format_audio(0.0), "—");
        assert_eq!(format_audio(32_728.0), "32.7 kHz");
        assert_eq!(format_bytes(300 * 1024 * 1024), "300 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024 / 2), "1.50 GB");
        assert_eq!(format_uptime(59_999), "59s");
        assert_eq!(format_uptime(61_000), "1m 01s");
        assert_eq!(format_uptime(3_660_000), "1h 01m");
        assert_eq!(actor_name(Actor::Mcp), "agent");
        assert!(
            (spark_ceiling(&[4.0, 5.0]) - 6.25).abs() < 1e-5,
            "slow sessions autoscale"
        );
        assert!(
            spark_ceiling(&[59.0]) > GUEST_REFRESH_HZ as f32,
            "full speed shows reference"
        );
        assert_eq!(spark_ceiling(&[]), 1.0);
    }
}
