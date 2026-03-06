use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// A simple text input widget with cursor support.
/// Supports both free-text and numeric-only modes.
#[derive(Debug, Clone)]
pub struct TextInput {
    pub value: String,
    pub cursor: usize,
    pub numeric_only: bool,
    pub label: String,
}

impl TextInput {
    pub fn new(label: &str, initial: &str, numeric_only: bool) -> Self {
        let cursor = initial.len();
        Self {
            value: initial.to_string(),
            cursor,
            numeric_only,
            label: label.to_string(),
        }
    }

    /// Handle a key event. Returns true if input was consumed.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c) => {
                if self.numeric_only && !c.is_ascii_digit() && c != '.' && c != '-' {
                    return true; // Consume but reject invalid chars
                }
                self.value.insert(self.cursor, c);
                self.cursor += 1;
                true
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.value.remove(self.cursor);
                }
                true
            }
            KeyCode::Delete => {
                if self.cursor < self.value.len() {
                    self.value.remove(self.cursor);
                }
                true
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.value.len());
                true
            }
            KeyCode::Home => {
                self.cursor = 0;
                true
            }
            KeyCode::End => {
                self.cursor = self.value.len();
                true
            }
            _ => false,
        }
    }

    /// Render the input field.
    pub fn render(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let style = if focused {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let cursor_style = Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow);

        // Build display with cursor
        let display = if focused && self.cursor <= self.value.len() {
            let (before, after) = self.value.split_at(self.cursor);
            let cursor_char = after.chars().next().unwrap_or(' ');
            let rest = if after.len() > 1 { &after[cursor_char.len_utf8()..] } else { "" };

            Line::from(vec![
                Span::styled(format!(" {}: ", self.label), Style::default().fg(Color::DarkGray)),
                Span::styled(before, style),
                Span::styled(cursor_char.to_string(), cursor_style),
                Span::styled(rest, style),
            ])
        } else {
            Line::from(vec![
                Span::styled(format!(" {}: ", self.label), Style::default().fg(Color::DarkGray)),
                Span::styled(&self.value, style),
            ])
        };

        frame.render_widget(Paragraph::new(display), area);
    }
}

/// A form field: label + current value + editing state.
#[derive(Debug, Clone)]
pub struct FormField {
    pub label: String,
    pub value: String,
    pub field_type: FieldType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    Decimal,
    Integer,
    Text,
}

impl FormField {
    pub fn decimal(label: &str, value: &str) -> Self {
        Self {
            label: label.to_string(),
            value: value.to_string(),
            field_type: FieldType::Decimal,
        }
    }

    pub fn integer(label: &str, value: &str) -> Self {
        Self {
            label: label.to_string(),
            value: value.to_string(),
            field_type: FieldType::Integer,
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self.field_type, FieldType::Decimal | FieldType::Integer)
    }

    /// Render a non-editing field row.
    pub fn render_row(&self, selected: bool) -> Line<'_> {
        let indicator = if selected { ">" } else { " " };
        let style = if selected {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        Line::from(vec![
            Span::styled(format!("{indicator} {}: ", self.label), Style::default().fg(Color::DarkGray)),
            Span::styled(&self.value, style),
        ])
    }
}

/// Render a list of form fields with optional title and border.
pub fn render_form(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    fields: &[FormField],
    selected: usize,
    editing: Option<&TextInput>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Each field gets one line
    let field_height = 1u16;
    for (i, field) in fields.iter().enumerate() {
        let y = inner.y + i as u16 * field_height;
        if y >= inner.y + inner.height {
            break;
        }

        let field_area = Rect::new(inner.x, y, inner.width, field_height);

        if Some(i) == editing.as_ref().map(|_| selected) {
            // Currently editing this field
            if let Some(input) = editing {
                input.render(frame, field_area, true);
            }
        } else {
            let line = field.render_row(i == selected);
            frame.render_widget(Paragraph::new(line), field_area);
        }
    }
}
