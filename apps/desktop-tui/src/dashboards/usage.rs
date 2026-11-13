//! The usage dashboard: token spend, when the numbers exist.
//!
//! The honest case is the interesting one. The CLI's `/usage` panel is a heatmap
//! of intensity buckets and reports totals only in aggregate, so there are no
//! per-hour, per-model or exact per-day numbers to chart. When the Host could
//! not produce the structured arrays, `panel` carries the CLI's own text and the
//! arrays are empty — and showing that text is the truthful rendering, while
//! charting the buckets as token counts would be a fabrication.

use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

use crate::model::Usage;

/// The widest bar in the set, so every other bar can be scaled against it.
///
/// Zero when nothing has a value: dividing by it would be a divide-by-zero, and
/// an all-zero set is the normal empty case rather than an error.
fn peak(values: &[u64]) -> u64 {
    values.iter().copied().max().unwrap_or(0)
}

pub fn render(frame: &mut Frame, model: &Usage) {
    let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(frame.area());

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("生成于 "),
            Span::raw(model.generated_at.clone()),
        ]))
        .block(Block::default().title("用量").borders(Borders::ALL)),
        chunks[0],
    );

    // The CLI's own panel, verbatim, is the answer when the structured arrays
    // are not there. It is shown rather than substituted for: the panel is real
    // data the user asked for.
    if let Some(panel) = &model.panel {
        frame.render_widget(
            Paragraph::new(panel.clone()).block(Block::default().borders(Borders::ALL)),
            chunks[1],
        );
        return;
    }

    if let Some(reason) = &model.unavailable_reason {
        // An honest empty with the reason, not a blank panel and not a fake one.
        frame.render_widget(
            Paragraph::new(reason.clone())
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL)),
            chunks[1],
        );
        return;
    }

    // Three areas, not two. The gauge used to share the model panel's area, and
    // whichever widget drew second won — so the busiest-hour gauge silently
    // replaced the per-model breakdown instead of sitting under it.
    let body = Layout::vertical([
        Constraint::Percentage(40),
        Constraint::Percentage(40),
        Constraint::Min(1),
    ])
    .split(chunks[1]);

    let per_day: Vec<u64> = model.days.iter().map(|day| day.tokens).collect();
    let widest = peak(&per_day);
    let day_rows: Vec<Line> = model
        .days
        .iter()
        .map(|day| {
            let ratio = if widest == 0 {
                0.0
            } else {
                day.tokens as f64 / widest as f64
            };
            Line::from(vec![
                Span::raw(format!("{} ", day.date)),
                Span::styled(
                    "█".repeat((ratio * 40.0) as usize),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(format!(" {}", day.tokens)),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(day_rows).block(Block::default().title("按日").borders(Borders::ALL)),
        body[0],
    );

    let per_model: Vec<u64> = model.models.iter().map(|model| model.tokens).collect();
    let widest_model = peak(&per_model);
    let model_rows: Vec<Line> = model
        .models
        .iter()
        .map(|entry| {
            let ratio = if widest_model == 0 {
                0.0
            } else {
                entry.tokens as f64 / widest_model as f64
            };
            Line::from(vec![
                Span::raw(format!("{} ", entry.model)),
                Span::styled(
                    "█".repeat((ratio * 30.0) as usize),
                    Style::default().fg(Color::Green),
                ),
                Span::raw(format!(" {}", entry.tokens)),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(model_rows).block(Block::default().title("按模型").borders(Borders::ALL)),
        body[1],
    );

    // A gauge for the busiest hour, so the shape of the day is visible without
    // reading 24 numbers.
    if let Some(busiest) = model
        .hours
        .iter()
        .max_by_key(|hour| hour.tokens)
        .filter(|hour| hour.tokens > 0)
    {
        let total: u64 = model.hours.iter().map(|hour| hour.tokens).sum();
        let ratio = if total == 0 {
            0.0
        } else {
            busiest.tokens as f64 / total as f64
        };
        frame.render_widget(
            Gauge::default()
                .block(Block::default().title("最忙时段").borders(Borders::ALL))
                .gauge_style(Style::default().fg(Color::Yellow))
                .ratio(ratio)
                .label(format!("{} 时 · {}", busiest.hour, busiest.tokens)),
            body[2],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::model::{Usage, UsageDay, UsageHour, UsageModel};

    fn render_to_string(model: &Usage) -> String {
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).expect("a terminal");
        terminal.draw(|frame| render(frame, model)).expect("draw");
        crate::dashboards::flatten(terminal.backend().buffer())
    }

    #[test]
    fn the_cli_panel_is_shown_when_the_structured_arrays_are_not_there() {
        // The honest case: the CLI's `/usage` panel is a heatmap of intensity
        // buckets with totals only in aggregate, so charting it as token counts
        // would be a fabrication. Passing the text through is the truthful
        // rendering.
        let model = Usage {
            generated_at: "2026-11-13T00:00:00.000Z".to_string(),
            days: Vec::new(),
            models: Vec::new(),
            by_model: Vec::new(),
            hours: Vec::new(),
            unavailable_reason: None,
            panel: Some("Total tokens: 1.2M".to_string()),
        };
        let text = render_to_string(&model);
        assert!(
            text.contains("Total tokens: 1.2M"),
            "the panel is shown: {text}"
        );
    }

    #[test]
    fn a_reason_is_shown_when_there_is_neither_panel_nor_arrays() {
        // An honest empty with the reason, not a blank panel and not a fake one.
        let model = Usage {
            generated_at: "2026-11-13T00:00:00.000Z".to_string(),
            days: Vec::new(),
            models: Vec::new(),
            by_model: Vec::new(),
            hours: Vec::new(),
            unavailable_reason: Some("CLI 未安装".to_string()),
            panel: None,
        };
        let text = render_to_string(&model);
        assert!(text.contains("CLI 未安装"), "the reason is shown: {text}");
    }

    #[test]
    fn the_arrays_are_charted_when_they_exist() {
        let model = Usage {
            generated_at: "2026-11-13T00:00:00.000Z".to_string(),
            days: vec![
                UsageDay {
                    date: "2026-11-11".to_string(),
                    tokens: 100,
                },
                UsageDay {
                    date: "2026-11-12".to_string(),
                    tokens: 300,
                },
            ],
            models: vec![UsageModel {
                model: "step-3".to_string(),
                tokens: 400,
            }],
            by_model: Vec::new(),
            hours: vec![
                UsageHour {
                    hour: 9,
                    tokens: 10,
                },
                UsageHour {
                    hour: 14,
                    tokens: 390,
                },
            ],
            unavailable_reason: None,
            panel: None,
        };
        let text = render_to_string(&model);
        assert!(text.contains("2026-11-12"), "{text}");
        assert!(text.contains("step-3"), "{text}");
        // The busiest hour is named rather than left to be counted.
        assert!(
            text.contains("14 时"),
            "the busiest hour is labelled: {text}"
        );
    }

    #[test]
    fn an_all_zero_set_renders_without_dividing_by_zero() {
        // Zero is the normal empty case, not an error.
        let model = Usage {
            generated_at: "2026-11-13T00:00:00.000Z".to_string(),
            days: vec![UsageDay {
                date: "2026-11-12".to_string(),
                tokens: 0,
            }],
            models: Vec::new(),
            by_model: Vec::new(),
            hours: vec![UsageHour { hour: 9, tokens: 0 }],
            unavailable_reason: None,
            panel: None,
        };
        let text = render_to_string(&model);
        assert!(text.contains("2026-11-12"), "{text}");
    }
}
