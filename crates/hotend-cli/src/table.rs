//! Plain text tables: columns padded to their widest cell, no colours, no box drawing, so
//! the output reads the same in a terminal, a log file and a bug report.

/// Formats `rows` under `header`. Every column but the last is padded; cells are used as
/// given (callers shorten long text themselves).
pub fn render(header: &[&str], rows: &[Vec<String>]) -> String {
    render_wrapped(header, rows, usize::MAX)
}

/// Like [`render`], with the last column wrapped at spaces so that lines stay within
/// `width` characters; continuation lines are indented to the last column.
pub fn render_wrapped(header: &[&str], rows: &[Vec<String>], width: usize) -> String {
    let columns = header.len();
    let mut widths: Vec<usize> = header.iter().map(|cell| cell.chars().count()).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate().take(columns) {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }
    let lead: usize = widths
        .iter()
        .take(columns.saturating_sub(1))
        .map(|w| w + 2)
        .sum();
    let indent = " ".repeat(lead);
    let room = width.saturating_sub(lead).max(24);
    let mut out = String::new();
    let mut line = |cells: Vec<&str>| {
        let mut text = String::new();
        for (index, cell) in cells.iter().enumerate().take(columns) {
            if index + 1 == columns {
                text.push_str(&wrap(cell, room, &indent));
            } else {
                text.push_str(cell);
                let pad = widths[index].saturating_sub(cell.chars().count()) + 2;
                text.extend(std::iter::repeat_n(' ', pad));
            }
        }
        for part in text.lines() {
            out.push_str(part.trim_end());
            out.push('\n');
        }
    };
    line(header.to_vec());
    let rule: Vec<String> = widths
        .iter()
        .enumerate()
        .map(|(index, width)| {
            "-".repeat(if index + 1 == columns {
                (*width).min(room)
            } else {
                *width
            })
        })
        .collect();
    line(rule.iter().map(String::as_str).collect());
    for row in rows {
        line(row.iter().map(String::as_str).collect());
    }
    out
}

/// Breaks `text` into lines of at most `width` characters at spaces, each after the first
/// indented by `indent`. Text that is already short stays on one line.
pub fn wrap(text: &str, width: usize, indent: &str) -> String {
    let mut out = String::new();
    for (number, paragraph) in text.lines().enumerate() {
        if number > 0 {
            out.push('\n');
            out.push_str(indent);
        }
        let mut column = 0;
        for word in paragraph.split(' ') {
            let length = word.chars().count();
            if column > 0 && column + 1 + length > width {
                out.push('\n');
                out.push_str(indent);
                column = 0;
            } else if column > 0 {
                out.push(' ');
                column += 1;
            }
            out.push_str(word);
            column += length;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_line_up_and_lines_have_no_trailing_spaces() {
        let text = render(
            &["check", "status", "detail"],
            &[
                vec!["sandbox".into(), "ok".into(), "landlock".into()],
                vec!["python environment".into(), "FAIL".into(), String::new()],
            ],
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "check               status  detail");
        assert_eq!(lines[1], "------------------  ------  --------");
        assert_eq!(lines[2], "sandbox             ok      landlock");
        assert_eq!(lines[3], "python environment  FAIL");
    }

    #[test]
    fn the_last_column_wraps_under_itself() {
        let text = render_wrapped(
            &["check", "detail"],
            &[vec![
                "sandbox".into(),
                "one two three four five six seven eight".into(),
            ]],
            34,
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[2], "sandbox  one two three four five");
        assert_eq!(lines[3], "         six seven eight");
    }

    #[test]
    fn long_text_wraps_at_spaces_with_the_indent() {
        let text = wrap("one two three four five", 9, "  ");
        assert_eq!(text, "one two\n  three\n  four five");
        assert_eq!(wrap("first\nsecond", 40, "> "), "first\n> second");
    }
}
