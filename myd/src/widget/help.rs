use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

/// Category of related keybindings for the help screen.
struct HelpCategory {
    title: &'static str,
    items: &'static [(&'static str, &'static str)],
}

/// Render a modal help overlay with a dimmed background and bordered box.
pub fn render_help(frame: &mut Frame, area: Rect) {
    // 1. Clear the area so our background can overlay everything.
    frame.render_widget(Clear, area);

    // 2. Draw a dimmed background over the full screen.
    let bg = Paragraph::new(Line::from("")).style(Style::default().bg(Color::Rgb(20, 20, 30)));
    frame.render_widget(bg, area);

    let categories: &[HelpCategory] = &[
        HelpCategory {
            title: "Navigation",
            items: &[
                ("j / Down", "Cursor down"),
                ("k / Up", "Cursor up"),
                ("gg", "Go to top"),
                ("G", "Go to bottom"),
                ("Ctrl+D", "Page down"),
                ("Ctrl+U", "Page up"),
                ("Enter", "Enter directory"),
                ("h / Left", "Collapse / Go back"),
                ("l / Right", "Expand directory"),
                ("Ctrl+O", "Go back (pop screen)"),
                ("v", "Toggle TREE/TREEMAP view"),
            ],
        },
        HelpCategory {
            title: "Tree",
            items: &[
                ("l / Right", "Expand selected directory"),
                ("h / Left", "Collapse / Go back"),
                ("*", "Expand all"),
                ("0", "Collapse all"),
            ],
        },
        HelpCategory {
            title: "Treemap",
            items: &[
                ("v", "Toggle TREE/TREEMAP view"),
                ("j / k / h / l", "Navigate treemap tiles"),
                ("G / gg", "First / Last tile"),
            ],
        },
        HelpCategory {
            title: "View",
            items: &[
                ("s", "Toggle sort order"),
                ("H", "Toggle hidden files"),
                ("b", "Toggle size bars"),
                ("Ctrl+B / t", "Toggle info panel"),
            ],
        },
        HelpCategory {
            title: "Actions",
            items: &[
                ("D", "Delete selected"),
                ("R", "Rename selected"),
                ("r", "Refresh"),
                ("/", "Search files"),
                ("gd", "Go to directory picker"),
            ],
        },
        HelpCategory {
            title: "Exit",
            items: &[
                ("q / Esc", "Quit immediately"),
            ],
        },
        HelpCategory {
            title: "Dialogs",
            items: &[
                ("Enter", "Confirm"),
                ("Esc", "Cancel"),
            ],
        },
    ];

    let total_items: usize = categories.iter().map(|c| c.items.len()).sum();
    let height = (2 + categories.len() * 2 + total_items) as u16;
    let height = height.min(area.height.saturating_sub(2));

    // Center the modal box vertically and horizontally.
    let outer =
        Layout::vertical([Constraint::Min(1), Constraint::Length(height), Constraint::Min(1)])
            .split(area);
    let box_area = Layout::horizontal([
        Constraint::Min(1),
        Constraint::Length(70),
        Constraint::Min(1),
    ])
    .split(outer[1]);
    let box_area = box_area[1];

    // 3. Draw the bordered modal box.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Line::from(Span::styled(
            " Help (? / F1 to toggle) ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));

    let mut lines = Vec::new();

    for cat in categories {
        lines.push(Line::from(Span::styled(
            format!("  {}:", cat.title),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));

        for (key, desc) in cat.items {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("    {:13}", key),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(*desc, Style::default().fg(Color::White)),
            ]));
        }

        lines.push(Line::from(""));
    }

    // Render the block (borders + title) then content inside.
    frame.render_widget(block.clone(), box_area);
    // Manually compute inner rect: account for border (1) and title (1).
    let inner = Rect::new(
        box_area.x + 1,
        box_area.y + 1,
        box_area.width - 2,
        box_area.height - 2,
    );
    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}
