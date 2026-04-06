use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::tui::app::{LoginField, LoginState};

pub fn render(frame: &mut Frame, state: &LoginState, area: Rect) {
    let block = Block::bordered().title(" Pod — Login ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Center the form vertically.
    let form_height = 11; // 3 fields * 3 lines each + 2 gaps
    let y_offset = inner.y + inner.height.saturating_sub(form_height) / 2;
    let x_offset = inner.x + inner.width.saturating_sub(50) / 2;
    let form_width = 50.min(inner.width);

    let fields = [
        ("Server URL", &state.server_url, LoginField::ServerUrl),
        ("Username", &state.username, LoginField::Username),
        ("Password", &state.password, LoginField::Password),
    ];

    for (i, (label, value, field)) in fields.iter().enumerate() {
        let y = y_offset + (i as u16) * 3;
        let is_active = state.active_field == *field;

        let label_style = if is_active {
            Style::default().fg(Color::Yellow).bold()
        } else {
            Style::default().fg(Color::Gray)
        };

        // Display password as dots.
        let display_value = if *field == LoginField::Password {
            "•".repeat(value.len())
        } else {
            value.to_string()
        };

        let input_style = if is_active {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let border_style = if is_active {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let label_area = Rect::new(x_offset, y, form_width, 1);
        frame.render_widget(
            Paragraph::new(*label).style(label_style),
            label_area,
        );

        let input_block = Block::bordered().border_style(border_style);
        let input_area = Rect::new(x_offset, y + 1, form_width, 3);
        let input_inner = input_block.inner(input_area);
        frame.render_widget(input_block, input_area);
        frame.render_widget(
            Paragraph::new(display_value).style(input_style),
            input_inner,
        );

        // Show cursor in active field.
        if is_active {
            frame.set_cursor_position((
                input_inner.x + value.len() as u16,
                input_inner.y,
            ));
        }
    }

    // Error message.
    if let Some(ref error) = state.error {
        let error_y = y_offset + 9;
        let error_area = Rect::new(x_offset, error_y, form_width, 1);
        frame.render_widget(
            Paragraph::new(error.as_str()).style(Style::default().fg(Color::Red)),
            error_area,
        );
    }

    // Loading indicator.
    if state.loading {
        let loading_y = y_offset + 10;
        let loading_area = Rect::new(x_offset, loading_y, form_width, 1);
        frame.render_widget(
            Paragraph::new("Logging in...").style(Style::default().fg(Color::Yellow)),
            loading_area,
        );
    }
}
