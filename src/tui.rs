use std::array;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};
use time::Weekday;

use crate::{
    app::{AppState, ViewMode},
    calendar::{CalendarCell, CalendarMonth, CalendarWeek, DAYS_PER_WEEK, MONTH_GRID_WEEKS},
    layout::ResponsiveLayout,
};

pub const DEFAULT_RENDER_WIDTH: u16 = 84;
pub const DEFAULT_RENDER_HEIGHT: u16 = 26;

const HEADER_HEIGHT: u16 = 2;
const VERTICAL_GRID_LINES: u16 = DAYS_PER_WEEK as u16 + 1;
const HORIZONTAL_GRID_LINES: u16 = MONTH_GRID_WEEKS as u16 + 1;

#[derive(Debug, Clone, Copy)]
pub struct MonthGrid<'a> {
    month: &'a CalendarMonth,
    styles: MonthGridStyles,
}

impl<'a> MonthGrid<'a> {
    pub const fn new(month: &'a CalendarMonth) -> Self {
        Self {
            month,
            styles: MonthGridStyles::new(),
        }
    }
}

impl Widget for MonthGrid<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_month_grid(self.month, area, buf, self.styles);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AppView<'a> {
    app: &'a AppState,
}

impl<'a> AppView<'a> {
    pub const fn new(app: &'a AppState) -> Self {
        Self { app }
    }
}

impl Widget for AppView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        match (self.app.view_mode(), ResponsiveLayout::for_area(area)) {
            (ViewMode::Month, layout) if layout.should_render_month_grid() => {
                let month = self.app.calendar_month();
                MonthGrid::new(&month).render(area, buf);
            }
            (ViewMode::Month, layout) if layout.should_render_week_view() => {
                let month = self.app.calendar_month();
                WeekGrid::new(&month).render(area, buf);
            }
            (ViewMode::Month | ViewMode::DayPlaceholder, _) => {
                render_day_placeholder(self.app, area, buf)
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WeekGrid<'a> {
    month: &'a CalendarMonth,
    styles: MonthGridStyles,
}

impl<'a> WeekGrid<'a> {
    pub const fn new(month: &'a CalendarMonth) -> Self {
        Self {
            month,
            styles: MonthGridStyles::new(),
        }
    }
}

impl Widget for WeekGrid<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_week_grid(self.month, area, buf, self.styles);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonthGridLayout {
    pub area: Rect,
    pub title_y: u16,
    pub weekday_y: u16,
    pub grid_area: Rect,
    column_widths: [u16; DAYS_PER_WEEK],
    row_heights: [u16; MONTH_GRID_WEEKS],
    column_bounds: [u16; DAYS_PER_WEEK + 1],
    row_bounds: [u16; MONTH_GRID_WEEKS + 1],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeekGridLayout {
    pub area: Rect,
    pub title_y: u16,
    pub weekday_y: u16,
    pub grid_area: Rect,
    column_widths: [u16; DAYS_PER_WEEK],
    column_bounds: [u16; DAYS_PER_WEEK + 1],
}

impl WeekGridLayout {
    pub fn new(area: Rect) -> Option<Self> {
        if area.width < VERTICAL_GRID_LINES || area.height < HEADER_HEIGHT + 2 {
            return None;
        }

        let grid_area = Rect::new(
            area.x,
            area.y + HEADER_HEIGHT,
            area.width,
            area.height - HEADER_HEIGHT,
        );
        let column_widths = distribute::<DAYS_PER_WEEK>(grid_area.width - VERTICAL_GRID_LINES);

        let mut column_bounds = [0; DAYS_PER_WEEK + 1];
        column_bounds[0] = grid_area.x;
        for index in 0..DAYS_PER_WEEK {
            column_bounds[index + 1] = column_bounds[index] + column_widths[index] + 1;
        }

        Some(Self {
            area,
            title_y: area.y,
            weekday_y: area.y + 1,
            grid_area,
            column_widths,
            column_bounds,
        })
    }

    pub fn cell_rect(&self, weekday_index: usize) -> Rect {
        Rect::new(
            self.column_bounds[weekday_index],
            self.grid_area.y,
            self.column_widths[weekday_index] + 2,
            self.grid_area.height,
        )
    }

    pub fn cell_content_rect(&self, weekday_index: usize) -> Rect {
        let cell = self.cell_rect(weekday_index);
        Rect::new(
            cell.x + 1,
            cell.y + 1,
            cell.width.saturating_sub(2),
            cell.height.saturating_sub(2),
        )
    }
}

impl MonthGridLayout {
    pub fn new(area: Rect) -> Option<Self> {
        if area.width < VERTICAL_GRID_LINES || area.height < HEADER_HEIGHT + HORIZONTAL_GRID_LINES {
            return None;
        }

        let grid_area = Rect::new(
            area.x,
            area.y + HEADER_HEIGHT,
            area.width,
            area.height - HEADER_HEIGHT,
        );
        let column_widths = distribute::<DAYS_PER_WEEK>(grid_area.width - VERTICAL_GRID_LINES);
        let row_heights = distribute::<MONTH_GRID_WEEKS>(grid_area.height - HORIZONTAL_GRID_LINES);

        let mut column_bounds = [0; DAYS_PER_WEEK + 1];
        column_bounds[0] = grid_area.x;
        for index in 0..DAYS_PER_WEEK {
            column_bounds[index + 1] = column_bounds[index] + column_widths[index] + 1;
        }

        let mut row_bounds = [0; MONTH_GRID_WEEKS + 1];
        row_bounds[0] = grid_area.y;
        for index in 0..MONTH_GRID_WEEKS {
            row_bounds[index + 1] = row_bounds[index] + row_heights[index] + 1;
        }

        Some(Self {
            area,
            title_y: area.y,
            weekday_y: area.y + 1,
            grid_area,
            column_widths,
            row_heights,
            column_bounds,
            row_bounds,
        })
    }

    pub fn cell_rect(&self, week_index: usize, weekday_index: usize) -> Rect {
        Rect::new(
            self.column_bounds[weekday_index],
            self.row_bounds[week_index],
            self.column_widths[weekday_index] + 2,
            self.row_heights[week_index] + 2,
        )
    }

    pub fn cell_content_rect(&self, week_index: usize, weekday_index: usize) -> Rect {
        let cell = self.cell_rect(week_index, weekday_index);
        Rect::new(
            cell.x + 1,
            cell.y + 1,
            cell.width.saturating_sub(2),
            cell.height.saturating_sub(2),
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct MonthGridStyles {
    title: Style,
    weekday: Style,
    selected: Style,
    selected_border: Style,
    today: Style,
    today_border: Style,
    in_month: Style,
    in_month_border: Style,
    filler: Style,
    filler_border: Style,
}

impl MonthGridStyles {
    const fn new() -> Self {
        Self {
            title: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            weekday: Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD),
            selected: Style::new()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            selected_border: Style::new()
                .fg(Color::Cyan)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            today: Style::new()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD.union(Modifier::DIM)),
            today_border: Style::new().fg(Color::Yellow).add_modifier(Modifier::DIM),
            in_month: Style::new().fg(Color::White).add_modifier(Modifier::DIM),
            in_month_border: Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            filler: Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            filler_border: Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        }
    }
}

pub fn render_month_to_string(month: &CalendarMonth, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    MonthGrid::new(month).render(area, &mut buffer);
    buffer_to_string(&buffer)
}

pub fn render_app_to_string(app: &AppState, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    AppView::new(app).render(area, &mut buffer);
    buffer_to_string(&buffer)
}

pub fn buffer_to_string(buffer: &Buffer) -> String {
    let mut output =
        String::with_capacity(usize::from(buffer.area.width + 1) * usize::from(buffer.area.height));

    for y in buffer.area.top()..buffer.area.bottom() {
        for x in buffer.area.left()..buffer.area.right() {
            let cell = buffer
                .cell((x, y))
                .expect("buffer iteration stays inside the buffer");
            output.push_str(cell.symbol());
        }
        output.push('\n');
    }

    output
}

fn render_month_grid(month: &CalendarMonth, area: Rect, buf: &mut Buffer, styles: MonthGridStyles) {
    let Some(layout) = MonthGridLayout::new(area) else {
        render_too_small_message(area, buf, styles);
        return;
    };

    buf.set_style(area, Style::default());
    render_title(month, &layout, buf, styles);
    render_weekdays(&layout, buf, styles);

    for cell in month.cells().filter(|cell| !cell.is_selected) {
        render_cell(cell, &layout, buf, styles);
    }

    if let Some(selected) = month.selected_cell() {
        render_cell(selected, &layout, buf, styles);
    }
}

fn render_week_grid(month: &CalendarMonth, area: Rect, buf: &mut Buffer, styles: MonthGridStyles) {
    let Some(layout) = WeekGridLayout::new(area) else {
        render_too_small_message(area, buf, styles);
        return;
    };

    let Some(selected_week) = selected_week(month) else {
        render_too_small_message(area, buf, styles);
        return;
    };

    buf.set_style(area, Style::default());
    render_week_title(selected_week, &layout, buf, styles);
    render_weekdays_for_week(&layout, buf, styles);

    for cell in selected_week.cells.iter().filter(|cell| !cell.is_selected) {
        render_week_cell(cell, &layout, buf, styles);
    }

    if let Some(selected) = selected_week.cells.iter().find(|cell| cell.is_selected) {
        render_week_cell(selected, &layout, buf, styles);
    }
}

fn selected_week(month: &CalendarMonth) -> Option<&CalendarWeek> {
    let selected = month.selected_cell()?;
    month.weeks.get(selected.week_index)
}

fn render_too_small_message(area: Rect, buf: &mut Buffer, styles: MonthGridStyles) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    write_centered(buf, area.y, area.x, area.width, "rcal", styles.title);

    if area.height > 1 {
        write_centered(
            buf,
            area.y + 1,
            area.x,
            area.width,
            "terminal too small",
            styles.in_month,
        );
    }
}

fn render_day_placeholder(app: &AppState, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let styles = MonthGridStyles::new();
    buf.set_style(area, Style::default());

    let selected = app.selected_date();
    let title = format!(
        "{} {}, {}",
        selected.month(),
        selected.day(),
        selected.year()
    );
    write_centered(buf, area.y, area.x, area.width, &title, styles.title);

    if area.height > 2 {
        write_centered(
            buf,
            area.y + 2,
            area.x,
            area.width,
            "No agenda loaded",
            styles.in_month,
        );
    }

    if area.height > 4 {
        write_centered(
            buf,
            area.y + 4,
            area.x,
            area.width,
            "Esc returns to month",
            styles.filler,
        );
    }
}

fn render_title(
    month: &CalendarMonth,
    layout: &MonthGridLayout,
    buf: &mut Buffer,
    styles: MonthGridStyles,
) {
    let title = format!("{} {}", month.current.month, month.current.year);
    write_centered(
        buf,
        layout.title_y,
        layout.area.x,
        layout.area.width,
        &title,
        styles.title,
    );
}

fn render_weekdays(layout: &MonthGridLayout, buf: &mut Buffer, styles: MonthGridStyles) {
    for (index, weekday) in weekday_labels().into_iter().enumerate() {
        let content = layout.cell_content_rect(0, index);
        write_centered(
            buf,
            layout.weekday_y,
            content.x,
            content.width,
            weekday,
            styles.weekday,
        );
    }
}

fn render_weekdays_for_week(layout: &WeekGridLayout, buf: &mut Buffer, styles: MonthGridStyles) {
    for (index, weekday) in weekday_labels().into_iter().enumerate() {
        let content = layout.cell_content_rect(index);
        write_centered(
            buf,
            layout.weekday_y,
            content.x,
            content.width,
            weekday,
            styles.weekday,
        );
    }
}

fn render_week_title(
    week: &CalendarWeek,
    layout: &WeekGridLayout,
    buf: &mut Buffer,
    styles: MonthGridStyles,
) {
    let title = week_title(week);
    write_centered(
        buf,
        layout.title_y,
        layout.area.x,
        layout.area.width,
        &title,
        styles.title,
    );
}

fn render_cell(
    cell: &CalendarCell,
    layout: &MonthGridLayout,
    buf: &mut Buffer,
    styles: MonthGridStyles,
) {
    let rect = layout.cell_rect(cell.week_index, cell.weekday_index);
    let content = layout.cell_content_rect(cell.week_index, cell.weekday_index);
    render_cell_in_rect(cell, rect, content, buf, styles);
}

fn render_week_cell(
    cell: &CalendarCell,
    layout: &WeekGridLayout,
    buf: &mut Buffer,
    styles: MonthGridStyles,
) {
    let rect = layout.cell_rect(cell.weekday_index);
    let content = layout.cell_content_rect(cell.weekday_index);
    render_cell_in_rect(cell, rect, content, buf, styles);
}

fn render_cell_in_rect(
    cell: &CalendarCell,
    rect: Rect,
    content: Rect,
    buf: &mut Buffer,
    styles: MonthGridStyles,
) {
    let (content_style, border_style, border_chars) = cell_style(cell, styles);

    buf.set_style(rect, content_style);
    draw_border(buf, rect, border_style, border_chars);

    if content.width == 0 || content.height == 0 {
        return;
    }

    let label = day_label(cell, content.width);
    let x = label_x(content, &label);
    buf.set_stringn(
        x,
        content.y,
        label,
        usize::from(content.width),
        content_style,
    );
}

fn week_title(week: &CalendarWeek) -> String {
    let first = week.cells[0].date;
    let last = week.cells[DAYS_PER_WEEK - 1].date;

    if first.year() != last.year() {
        return format!(
            "{} {}, {} - {} {}, {}",
            first.month(),
            first.day(),
            first.year(),
            last.month(),
            last.day(),
            last.year()
        );
    }

    if first.month() != last.month() {
        return format!(
            "{} {} - {} {}, {}",
            first.month(),
            first.day(),
            last.month(),
            last.day(),
            last.year()
        );
    }

    format!(
        "{} {}-{}, {}",
        first.month(),
        first.day(),
        last.day(),
        last.year()
    )
}

fn cell_style(cell: &CalendarCell, styles: MonthGridStyles) -> (Style, Style, BorderCharacters) {
    if cell.is_selected {
        return (
            styles.selected,
            styles.selected_border,
            BorderCharacters::selected(),
        );
    }

    if cell.is_today {
        return (styles.today, styles.today_border, BorderCharacters::today());
    }

    if cell.is_in_visible_month {
        return (
            styles.in_month,
            styles.in_month_border,
            BorderCharacters::normal(),
        );
    }

    (
        styles.filler,
        styles.filler_border,
        BorderCharacters::normal(),
    )
}

fn day_label(cell: &CalendarCell, width: u16) -> String {
    let mut label = cell.date.day().to_string();
    if cell.is_today {
        label.push('*');
    }

    if cell.is_selected && usize::from(width) >= label.len() + 2 {
        label = format!("[{label}]");
    }

    label
}

fn label_x(content: Rect, label: &str) -> u16 {
    let label_width = u16::try_from(label.len()).unwrap_or(u16::MAX);
    if content.width > label_width + 1 {
        content.x + 1
    } else {
        content.x
    }
}

#[derive(Debug, Clone, Copy)]
struct BorderCharacters {
    horizontal: &'static str,
    vertical: &'static str,
    corner: &'static str,
}

impl BorderCharacters {
    const fn normal() -> Self {
        Self {
            horizontal: "-",
            vertical: "|",
            corner: "+",
        }
    }

    const fn today() -> Self {
        Self {
            horizontal: "=",
            vertical: "!",
            corner: "+",
        }
    }

    const fn selected() -> Self {
        Self {
            horizontal: "#",
            vertical: "#",
            corner: "#",
        }
    }
}

fn draw_border(buf: &mut Buffer, rect: Rect, style: Style, chars: BorderCharacters) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }

    let left = rect.left();
    let right = rect.right().saturating_sub(1);
    let top = rect.top();
    let bottom = rect.bottom().saturating_sub(1);

    for x in left..=right {
        set_cell(buf, x, top, chars.horizontal, style);
        set_cell(buf, x, bottom, chars.horizontal, style);
    }

    for y in top..=bottom {
        set_cell(buf, left, y, chars.vertical, style);
        set_cell(buf, right, y, chars.vertical, style);
    }

    set_cell(buf, left, top, chars.corner, style);
    set_cell(buf, right, top, chars.corner, style);
    set_cell(buf, left, bottom, chars.corner, style);
    set_cell(buf, right, bottom, chars.corner, style);
}

fn set_cell(buf: &mut Buffer, x: u16, y: u16, symbol: &str, style: Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_symbol(symbol).set_style(style);
    }
}

fn write_centered(buf: &mut Buffer, y: u16, x: u16, width: u16, text: &str, style: Style) {
    if width == 0 || !buf.area.contains((x, y).into()) {
        return;
    }

    let text_width = u16::try_from(text.len()).unwrap_or(u16::MAX);
    let start = x + width.saturating_sub(text_width) / 2;
    buf.set_stringn(start, y, text, usize::from(width), style);
}

fn weekday_labels() -> [&'static str; DAYS_PER_WEEK] {
    array::from_fn(|index| match index {
        0 => weekday_label(Weekday::Sunday),
        1 => weekday_label(Weekday::Monday),
        2 => weekday_label(Weekday::Tuesday),
        3 => weekday_label(Weekday::Wednesday),
        4 => weekday_label(Weekday::Thursday),
        5 => weekday_label(Weekday::Friday),
        6 => weekday_label(Weekday::Saturday),
        _ => unreachable!("weekday index stays in range"),
    })
}

const fn weekday_label(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Sunday => "Sun",
        Weekday::Monday => "Mon",
        Weekday::Tuesday => "Tue",
        Weekday::Wednesday => "Wed",
        Weekday::Thursday => "Thu",
        Weekday::Friday => "Fri",
        Weekday::Saturday => "Sat",
    }
}

fn distribute<const N: usize>(total: u16) -> [u16; N] {
    let base = total / N as u16;
    let extra = usize::from(total % N as u16);
    array::from_fn(|index| base + u16::from(index < extra))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Modifier;
    use time::Month;

    use crate::calendar::CalendarDate;

    fn date(year: i32, month: Month, day: u8) -> CalendarDate {
        CalendarDate::from_ymd(year, month, day).expect("valid test date")
    }

    fn render_test_buffer(month: &CalendarMonth, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        MonthGrid::new(month).render(area, &mut buffer);
        buffer
    }

    fn buffer_lines(buffer: &Buffer) -> Vec<String> {
        buffer_to_string(buffer)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn cell_for_day(month: &CalendarMonth, day: u8) -> &CalendarCell {
        month
            .cells()
            .find(|cell| cell.is_in_visible_month && cell.date.day() == day)
            .expect("day exists in month")
    }

    #[test]
    fn fixed_month_grid_renders_stable_ascii_snapshot() {
        let month = CalendarMonth::for_launch_date(date(2026, Month::April, 23));
        let buffer = render_test_buffer(&month, 49, 20);
        let lines = buffer_lines(&buffer);

        assert_eq!(lines.len(), 20);
        assert_eq!(
            lines[0],
            "                   April 2026                    "
        );
        assert_eq!(
            lines[1],
            "  Sun    Mon    Tue    Wed    Thu    Fri    Sat  "
        );
        assert!(lines.join("\n").contains("[23*]"));
        assert!(lines.join("\n").contains("29"));
        assert!(lines.join("\n").contains("+------"));
    }

    #[test]
    fn layout_uses_available_area_for_cell_sizing() {
        let narrow = MonthGridLayout::new(Rect::new(0, 0, 49, 20)).expect("supported layout");
        let wide = MonthGridLayout::new(Rect::new(0, 0, 84, 26)).expect("supported layout");

        assert_eq!(wide.grid_area.right(), 84);
        assert_eq!(wide.grid_area.bottom(), 26);
        assert!(wide.cell_content_rect(0, 0).width > narrow.cell_content_rect(0, 0).width);
        assert!(wide.cell_content_rect(0, 0).height > narrow.cell_content_rect(5, 0).height);
    }

    #[test]
    fn active_today_and_filler_styles_are_distinct() {
        let month =
            CalendarMonth::from_dates(date(2026, Month::April, 18), date(2026, Month::April, 23));
        let buffer = render_test_buffer(&month, 84, 26);
        let layout = MonthGridLayout::new(Rect::new(0, 0, 84, 26)).expect("supported layout");

        let selected = cell_for_day(&month, 18);
        let selected_content =
            layout.cell_content_rect(selected.week_index, selected.weekday_index);
        let selected_cell = buffer
            .cell((selected_content.x + 1, selected_content.y))
            .expect("selected label cell exists");
        assert_eq!(selected_cell.bg, Color::Blue);
        assert_eq!(selected_cell.fg, Color::White);
        assert!(selected_cell.modifier.contains(Modifier::BOLD));
        assert!(!selected_cell.modifier.contains(Modifier::DIM));

        let today = cell_for_day(&month, 23);
        let today_content = layout.cell_content_rect(today.week_index, today.weekday_index);
        let today_cell = buffer
            .cell((today_content.x + 1, today_content.y))
            .expect("today label cell exists");
        assert_eq!(today_cell.fg, Color::Yellow);
        assert!(today_cell.modifier.contains(Modifier::BOLD));
        assert!(today_cell.modifier.contains(Modifier::DIM));

        let filler_cell = buffer.cell((2, 3)).expect("filler label cell exists");
        assert_eq!(filler_cell.fg, Color::DarkGray);
        assert!(filler_cell.modifier.contains(Modifier::DIM));
    }

    #[test]
    fn active_cell_has_focus_perimeter() {
        let month = CalendarMonth::for_launch_date(date(2026, Month::April, 23));
        let buffer = render_test_buffer(&month, 84, 26);
        let layout = MonthGridLayout::new(Rect::new(0, 0, 84, 26)).expect("supported layout");
        let selected = month.selected_cell().expect("selected cell exists");
        let rect = layout.cell_rect(selected.week_index, selected.weekday_index);

        let top_left = buffer
            .cell((rect.x, rect.y))
            .expect("selected border exists");
        let top_edge = buffer
            .cell((rect.x + 1, rect.y))
            .expect("selected border exists");
        let left_edge = buffer
            .cell((rect.x, rect.y + 1))
            .expect("selected border exists");

        assert_eq!(top_left.symbol(), "#");
        assert_eq!(top_edge.symbol(), "#");
        assert_eq!(left_edge.symbol(), "#");
        assert_eq!(top_left.fg, Color::Cyan);
        assert_eq!(top_left.bg, Color::Blue);
        assert!(top_left.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn small_supported_month_grid_still_shows_month_and_selection() {
        let month = CalendarMonth::for_launch_date(date(2026, Month::April, 23));
        let rendered = render_month_to_string(&month, 49, 20);

        assert!(rendered.contains("April 2026"));
        assert!(rendered.contains("Sun"));
        assert!(rendered.contains("[23*]"));
    }

    #[test]
    fn app_view_prefers_month_grid_when_full_month_fits() {
        let app = AppState::new(date(2026, Month::April, 23));
        let rendered = render_app_to_string(&app, 49, 20);

        assert!(rendered.contains("April 2026"));
        assert!(rendered.contains("Sun"));
        assert!(rendered.contains("[23*]"));
        assert!(rendered.contains("30"));
    }

    #[test]
    fn app_view_uses_week_fallback_under_height_pressure() {
        let app = AppState::new(date(2026, Month::April, 23));
        let rendered = render_app_to_string(&app, 84, 8);

        assert!(rendered.contains("April 19-25, 2026"));
        assert!(rendered.contains("Sun"));
        assert!(rendered.contains("[23*]"));
        assert!(!rendered.contains("April 2026"));
        assert!(!rendered.contains("29"));
    }

    #[test]
    fn app_view_uses_day_fallback_when_week_cannot_fit() {
        let app = AppState::new(date(2026, Month::April, 23));
        let rendered = render_app_to_string(&app, 35, 10);

        assert!(rendered.contains("April 23, 2026"));
        assert!(rendered.contains("No agenda loaded"));
        assert!(!rendered.contains("Sun"));
    }

    #[test]
    fn constrained_week_fallback_has_stable_ascii_snapshot() {
        let app = AppState::new(date(2026, Month::April, 23));
        let rendered = render_app_to_string(&app, 36, 6);
        let lines: Vec<_> = rendered.lines().collect();

        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], "         April 19-25, 2026          ");
        assert_eq!(lines[1], " Sun  Mon  Tue  Wed  Thu  Fri  Sat  ");
        assert!(rendered.contains("23*"));
        assert!(rendered.contains("#"));
        assert!(rendered.contains("+----+"));
    }

    #[test]
    fn responsive_resize_recomputes_without_changing_selection() {
        let app = AppState::from_dates(date(2026, Month::April, 18), date(2026, Month::April, 23));
        let selected = app.selected_date();

        let month = render_app_to_string(&app, 84, 26);
        let week = render_app_to_string(&app, 84, 8);
        let day = render_app_to_string(&app, 35, 10);

        assert_eq!(app.selected_date(), selected);
        assert!(month.contains("April 2026"));
        assert!(month.contains("[18]"));
        assert!(week.contains("April 12-18, 2026"));
        assert!(week.contains("[18]"));
        assert!(day.contains("April 18, 2026"));
    }

    #[test]
    fn app_view_renders_day_placeholder() {
        let app = {
            let mut app = AppState::new(date(2026, Month::April, 23));
            app.apply(crate::app::AppAction::OpenDay);
            app
        };
        let area = Rect::new(0, 0, 49, 10);
        let mut buffer = Buffer::empty(area);

        AppView::new(&app).render(area, &mut buffer);
        let rendered = buffer_to_string(&buffer);

        assert!(rendered.contains("April 23, 2026"));
        assert!(rendered.contains("No agenda loaded"));
        assert!(!rendered.contains("April 2026"));
    }
}
