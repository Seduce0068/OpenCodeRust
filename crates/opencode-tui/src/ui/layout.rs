use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub fn horizontal_split(area: Rect, constraints: Vec<Constraint>) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area)
        .to_vec()
}

pub fn vertical_split(area: Rect, constraints: Vec<Constraint>) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area)
        .to_vec()
}

pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height) / 2),
            Constraint::Length(height),
            Constraint::Percentage((100 - height) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width) / 2),
            Constraint::Length(width),
            Constraint::Percentage((100 - width) / 2),
        ])
        .split(popup_layout[1])[1]
}

pub fn footer(area: Rect, height: u16) -> Rect {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(height)])
        .split(area);
    layout[1]
}

pub fn header(area: Rect, height: u16) -> Rect {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(height), Constraint::Min(0)])
        .split(area);
    layout[0]
}

pub fn sidebar_left(area: Rect, width: u16) -> (Rect, Rect) {
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(width), Constraint::Min(0)])
        .split(area);
    (layout[0], layout[1])
}

pub fn sidebar_right(area: Rect, width: u16) -> (Rect, Rect) {
    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(width)])
        .split(area);
    (layout[0], layout[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 100, 50)
    }

    #[test]
    fn horizontal_split_respects_lengths() {
        let parts = horizontal_split(area(), vec![Constraint::Length(20), Constraint::Min(0)]);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].width, 20);
        assert_eq!(parts[1].width, 80);
        assert_eq!(parts[0].x, 0);
        assert_eq!(parts[1].x, 20);
    }

    #[test]
    fn vertical_split_respects_lengths() {
        let parts = vertical_split(area(), vec![Constraint::Length(10), Constraint::Min(0)]);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].height, 10);
        assert_eq!(parts[1].height, 40);
        assert_eq!(parts[1].y, 10);
    }

    #[test]
    fn header_and_footer_are_at_edges() {
        assert_eq!(header(area(), 5).y, 0);
        assert_eq!(header(area(), 5).height, 5);

        let foot = footer(area(), 5);
        assert_eq!(foot.y, 45);
        assert_eq!(foot.height, 5);
    }

    #[test]
    fn sidebars_take_requested_width() {
        let (left, main) = sidebar_left(area(), 30);
        assert_eq!(left.width, 30);
        assert_eq!(main.x, 30);

        let (main_right, right) = sidebar_right(area(), 30);
        assert_eq!(right.width, 30);
        assert_eq!(main_right.width, 70);
    }

    #[test]
    fn centered_is_inside_area() {
        let popup = centered(area(), 40, 20);
        assert_eq!(popup.width, 40);
        assert_eq!(popup.height, 20);
        assert!(popup.x + popup.width <= 100);
        assert!(popup.y + popup.height <= 50);
    }
}
