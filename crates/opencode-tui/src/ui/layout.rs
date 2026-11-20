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
    // Clamp the requested size to the area first so the centering math can
    // never underflow. The previous implementation fed `100 - height` and
    // `100 - width` into `Constraint::Percentage`, which panicked in debug
    // builds (and produced a huge value in release) whenever a dimension
    // exceeded 100.
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(height) / 2);
    Rect::new(x, y, width, height)
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

    #[test]
    fn centered_clamps_dimensions_larger_than_area() {
        // Regression: the old Percentage((100 - height) / 2) underflowed when
        // height/width exceeded 100. The result must stay inside `area`.
        let popup = centered(area(), 150, 120);
        assert_eq!(popup.width, 100);
        assert_eq!(popup.height, 50);
        assert_eq!(popup.x, 0);
        assert_eq!(popup.y, 0);

        let just_over = centered(area(), 101, 101);
        assert_eq!(just_over.width, 100);
        assert_eq!(just_over.height, 50);
        assert!(just_over.x + just_over.width <= 100);
        assert!(just_over.y + just_over.height <= 50);
    }

    #[test]
    fn zero_sized_area_does_not_panic() {
        let popup = centered(Rect::new(0, 0, 0, 0), 40, 20);
        assert_eq!(popup.width, 0);
        assert_eq!(popup.height, 0);
    }

    #[test]
    fn centered_centers_within_nonzero_origin() {
        let region = Rect::new(10, 5, 40, 20);
        let popup = centered(region, 20, 10);
        assert_eq!(popup.x, 20);
        assert_eq!(popup.y, 10);
        assert_eq!(popup.width, 20);
        assert_eq!(popup.height, 10);
        assert!(popup.x + popup.width <= region.x + region.width);
        assert!(popup.y + popup.height <= region.y + region.height);
    }
}
