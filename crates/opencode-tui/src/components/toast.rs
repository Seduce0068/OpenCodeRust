use ratatui::{
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

const TRANSITION_MS: u64 = 120;
const SLIDE_OFFSET_COLS: u16 = 4;
const MIN_FADE_ALPHA: f32 = 0.45;
const MAX_TOAST_HEIGHT: u16 = 6;
const MIN_TOAST_HEIGHT: u16 = 3;
// 2 columns for the left/right borders plus 2 columns of spare room.
const TOAST_WIDTH_OVERHEAD: u16 = 4;
// Vertical padding kept below the text so it does not stick to the border.
const TOAST_HEIGHT_OVERHEAD: u16 = 2;

pub struct Toast {
    message: Option<String>,
    variant: ToastVariant,
    duration_ms: u64,
    phase: ToastPhase,
    phase_elapsed_ms: u64,
}

#[derive(Clone, Copy)]
pub enum ToastVariant {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToastPhase {
    Entering,
    Visible,
    Exiting,
}

impl Toast {
    pub fn new() -> Self {
        Self {
            message: None,
            variant: ToastVariant::Info,
            duration_ms: 3000,
            phase: ToastPhase::Entering,
            phase_elapsed_ms: 0,
        }
    }

    pub fn show(&mut self, variant: ToastVariant, message: &str, duration_ms: u64) {
        self.variant = variant;
        self.message = Some(message.to_string());
        self.duration_ms = duration_ms;
        self.phase = ToastPhase::Entering;
        self.phase_elapsed_ms = 0;
    }

    pub fn tick(&mut self, delta_ms: u64) -> bool {
        if self.message.is_none() {
            return false;
        }

        let previous_phase = self.phase;
        let previous_elapsed = self.phase_elapsed_ms;
        let had_message = self.message.is_some();

        self.phase_elapsed_ms = self.phase_elapsed_ms.saturating_add(delta_ms);
        match self.phase {
            ToastPhase::Entering if self.phase_elapsed_ms >= TRANSITION_MS => {
                self.phase = ToastPhase::Visible;
                self.phase_elapsed_ms = 0;
            }
            ToastPhase::Visible if self.phase_elapsed_ms >= self.duration_ms => {
                self.phase = ToastPhase::Exiting;
                self.phase_elapsed_ms = 0;
            }
            ToastPhase::Exiting if self.phase_elapsed_ms >= TRANSITION_MS => {
                self.message = None;
                self.phase = ToastPhase::Entering;
                self.phase_elapsed_ms = 0;
            }
            _ => {}
        }

        if self.phase != previous_phase || self.message.is_some() != had_message {
            return true;
        }

        matches!(self.phase, ToastPhase::Entering | ToastPhase::Exiting)
            && self.phase_elapsed_ms != previous_elapsed
    }

    pub fn is_visible(&self) -> bool {
        self.message.is_some()
    }

    pub fn slide_offset(&self) -> u16 {
        let progress = self.animation_progress();
        let hidden = (1.0 - progress) * SLIDE_OFFSET_COLS as f32;
        hidden.round() as u16
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        let Some(msg) = self.message.as_ref() else {
            return 0;
        };
        if width == 0 {
            return 0;
        }
        let lines = wrapped_line_count(msg, text_width_for(width));
        (lines + TOAST_HEIGHT_OVERHEAD).clamp(MIN_TOAST_HEIGHT, MAX_TOAST_HEIGHT)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if let Some(msg) = &self.message {
            let progress = self.animation_progress();
            let fade_alpha = MIN_FADE_ALPHA + (1.0 - MIN_FADE_ALPHA) * progress;
            let border_color = match self.variant {
                ToastVariant::Info => theme.info,
                ToastVariant::Success => theme.success,
                ToastVariant::Warning => theme.warning,
                ToastVariant::Error => theme.error,
            };
            let text_color = blend_colors(theme.text_muted, theme.text, progress);
            let panel_bg = blend_colors(theme.background, theme.background_panel, fade_alpha);
            let border_color = blend_colors(theme.background_panel, border_color, fade_alpha);

            // Keep the rendered text within the height budget returned by
            // `desired_height`. Overflow is an intentional ellipsis rather
            // than a silent clip performed by the terminal buffer.
            let max_content_lines =
                usize::from(MAX_TOAST_HEIGHT.saturating_sub(TOAST_HEIGHT_OVERHEAD)).max(1);
            let shown = fit_message(msg, text_width_for(area.width), max_content_lines);
            let paragraph = Paragraph::new(shown)
                .style(Style::default().fg(text_color).bg(panel_bg))
                .block(
                    Block::default()
                        .borders(Borders::LEFT | Borders::RIGHT)
                        .border_style(Style::default().fg(border_color))
                        .style(Style::default().bg(panel_bg)),
                )
                .wrap(Wrap { trim: true });
            frame.render_widget(paragraph, area);
        }
    }

    fn animation_progress(&self) -> f32 {
        if self.message.is_none() {
            return 0.0;
        }

        match self.phase {
            ToastPhase::Entering => progress_ratio(self.phase_elapsed_ms, TRANSITION_MS),
            ToastPhase::Visible => 1.0,
            ToastPhase::Exiting => 1.0 - progress_ratio(self.phase_elapsed_ms, TRANSITION_MS),
        }
        .clamp(0.0, 1.0)
    }
}

impl Default for Toast {
    fn default() -> Self {
        Self::new()
    }
}

/// Usable text width after reserving room for the side borders and padding.
fn text_width_for(width: u16) -> usize {
    usize::from(width.saturating_sub(TOAST_WIDTH_OVERHEAD)).max(1)
}

/// Count wrapped lines using terminal display columns, not Unicode scalar
/// values. Full-width (e.g. CJK) characters occupy two columns, so counting
/// `chars()` underestimates the height and clips the message tail.
fn wrapped_line_count(msg: &str, text_width: usize) -> u16 {
    let lines = UnicodeWidthStr::width(msg).div_ceil(text_width).max(1);
    // Clamp before the u16 cast so an extremely long message cannot wrap
    // around during conversion.
    lines.min(usize::from(MAX_TOAST_HEIGHT)) as u16
}

/// Trim `msg` to at most `max_lines * text_width` display columns, appending
/// `…` when trimming actually happens. Trimming stops on char boundaries so a
/// wide character is never split in half.
fn fit_message(msg: &str, text_width: usize, max_lines: usize) -> String {
    let budget = text_width.saturating_mul(max_lines);
    if UnicodeWidthStr::width(msg) <= budget {
        return msg.to_string();
    }

    let ellipsis = "…";
    let limit = budget.saturating_sub(UnicodeWidthStr::width(ellipsis));
    let mut result = String::new();
    let mut used = 0usize;
    for ch in msg.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1);
        if used.saturating_add(ch_width) > limit {
            break;
        }
        result.push(ch);
        used += ch_width;
    }
    result.push_str(ellipsis);
    result
}

fn progress_ratio(elapsed_ms: u64, total_ms: u64) -> f32 {
    if total_ms == 0 {
        return 1.0;
    }
    (elapsed_ms as f32 / total_ms as f32).clamp(0.0, 1.0)
}

fn blend_colors(from: Color, to: Color, alpha: f32) -> Color {
    let alpha = alpha.clamp(0.0, 1.0);
    let (fr, fg, fb) = color_to_rgb(from);
    let (tr, tg, tb) = color_to_rgb(to);
    Color::Rgb(
        mix_u8(fr, tr, alpha),
        mix_u8(fg, tg, alpha),
        mix_u8(fb, tb, alpha),
    )
}

fn mix_u8(from: u8, to: u8, alpha: f32) -> u8 {
    let from = from as f32;
    let to = to as f32;
    (from + (to - from) * alpha).round().clamp(0.0, 255.0) as u8
}

fn color_to_rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Black => (0, 0, 0),
        Color::Red => (205, 49, 49),
        Color::Green => (13, 188, 121),
        Color::Yellow => (229, 229, 16),
        Color::Blue => (36, 114, 200),
        Color::Magenta => (188, 63, 188),
        Color::Cyan => (17, 168, 205),
        Color::Gray => (204, 204, 204),
        Color::DarkGray => (118, 118, 118),
        Color::LightRed => (241, 76, 76),
        Color::LightGreen => (35, 209, 139),
        Color::LightYellow => (245, 245, 67),
        Color::LightBlue => (59, 142, 234),
        Color::LightMagenta => (214, 112, 214),
        Color::LightCyan => (41, 184, 219),
        Color::White => (255, 255, 255),
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(v) => (v, v, v),
        Color::Reset => (0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_transitions_enter_visible_exit_and_hides() {
        let mut toast = Toast::new();
        toast.show(ToastVariant::Info, "hello", 120);
        assert!(toast.is_visible());

        toast.tick(TRANSITION_MS);
        assert_eq!(toast.phase, ToastPhase::Visible);
        assert!(toast.is_visible());

        toast.tick(120);
        assert_eq!(toast.phase, ToastPhase::Exiting);
        assert!(toast.is_visible());

        toast.tick(TRANSITION_MS);
        assert!(!toast.is_visible());
    }

    #[test]
    fn toast_slide_offset_drops_to_zero_when_fully_visible() {
        let mut toast = Toast::new();
        toast.show(ToastVariant::Info, "hello", 3000);
        let start = toast.slide_offset();
        assert!(start > 0);
        toast.tick(TRANSITION_MS);
        assert_eq!(toast.slide_offset(), 0);
    }

    #[test]
    fn progress_ratio_clamps_and_handles_zero_total() {
        assert_eq!(progress_ratio(0, 0), 1.0);
        assert_eq!(progress_ratio(500, 1000), 0.5);
        assert_eq!(progress_ratio(2000, 1000), 1.0);
        assert_eq!(progress_ratio(100, 1000), 0.1);
    }

    #[test]
    fn mix_u8_interpolates_and_clamps_alpha() {
        assert_eq!(mix_u8(0, 100, 0.0), 0);
        assert_eq!(mix_u8(0, 100, 1.0), 100);
        assert_eq!(mix_u8(0, 100, 0.5), 50);
        assert_eq!(mix_u8(100, 0, 2.0), 0);
    }

    #[test]
    fn blend_colors_combines_rgb() {
        assert_eq!(
            blend_colors(Color::Rgb(0, 0, 0), Color::Rgb(100, 100, 100), 0.5),
            Color::Rgb(50, 50, 50)
        );
    }

    #[test]
    fn color_to_rgb_named_colors() {
        assert_eq!(color_to_rgb(Color::Black), (0, 0, 0));
        assert_eq!(color_to_rgb(Color::Red), (205, 49, 49));
        assert_eq!(color_to_rgb(Color::Green), (13, 188, 121));
    }

    #[test]
    fn desired_height_measures_full_width_chars_by_display_width() {
        let mut wide = Toast::new();
        let mut narrow = Toast::new();
        let wide_msg = "あ".repeat(40); // display width 80
        let narrow_msg = "a".repeat(40); // display width 40
        wide.show(ToastVariant::Info, &wide_msg, 3000);
        narrow.show(ToastVariant::Info, &narrow_msg, 3000);
        // width 24 -> text_width 20 -> wide: ceil(80/20)=4 lines, narrow: 2 lines.
        assert_eq!(wide.desired_height(24), 6);
        assert_eq!(narrow.desired_height(24), 4);
        assert!(wide.desired_height(24) > narrow.desired_height(24));
    }

    #[test]
    fn desired_height_empty_message_and_zero_width() {
        let toast = Toast::new();
        assert_eq!(toast.desired_height(24), 0);

        let mut toast = Toast::new();
        toast.show(ToastVariant::Info, "hello", 3000);
        assert_eq!(toast.desired_height(0), 0);
        assert_eq!(toast.desired_height(24), 3);
    }

    #[test]
    fn desired_height_never_exceeds_max() {
        let mut toast = Toast::new();
        let long = "あ".repeat(500);
        toast.show(ToastVariant::Info, &long, 3000);
        assert_eq!(toast.desired_height(24), MAX_TOAST_HEIGHT);
    }

    #[test]
    fn fit_message_truncates_on_char_boundary_with_ellipsis() {
        let msg = "あ".repeat(100); // display width 200
        let fitted = fit_message(&msg, 20, 4); // budget 80
        assert!(fitted.ends_with('…'));
        assert!(UnicodeWidthStr::width(fitted.as_str()) <= 80);
        // The ellipsis replaces the tail; no partial wide char is produced.
        assert!(fitted.chars().all(|c| c == 'あ' || c == '…'));
    }

    #[test]
    fn fit_message_keeps_text_within_budget_unchanged() {
        let msg = "あ".repeat(10); // display width 20
        assert_eq!(fit_message(&msg, 20, 4), msg);
    }
}
