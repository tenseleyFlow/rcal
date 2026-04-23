use std::array;

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    widgets::Widget,
};
use time::Weekday;

use crate::{
    agenda::{
        AgendaSource, DayAgenda, DayMinute, EmptyAgendaSource, Event, EventTiming, TimedAgendaEvent,
    },
    app::{AppState, CreateEventForm, ViewMode},
    calendar::{
        CalendarCell, CalendarDate, CalendarMonth, CalendarWeek, DAYS_PER_WEEK, MONTH_GRID_WEEKS,
    },
    layout::ResponsiveLayout,
};

pub const DEFAULT_RENDER_WIDTH: u16 = 84;
pub const DEFAULT_RENDER_HEIGHT: u16 = 26;

const HEADER_HEIGHT: u16 = 2;
const VERTICAL_GRID_LINES: u16 = DAYS_PER_WEEK as u16 + 1;
const HORIZONTAL_GRID_LINES: u16 = MONTH_GRID_WEEKS as u16 + 1;
static EMPTY_AGENDA_SOURCE: EmptyAgendaSource = EmptyAgendaSource;

#[derive(Clone, Copy)]
pub struct MonthGrid<'a> {
    month: &'a CalendarMonth,
    agenda_source: &'a dyn AgendaSource,
    styles: MonthGridStyles,
}

impl<'a> MonthGrid<'a> {
    pub fn new(month: &'a CalendarMonth) -> Self {
        Self::with_agenda_source(month, &EMPTY_AGENDA_SOURCE)
    }

    pub fn with_agenda_source(
        month: &'a CalendarMonth,
        agenda_source: &'a dyn AgendaSource,
    ) -> Self {
        Self {
            month,
            agenda_source,
            styles: MonthGridStyles::new(),
        }
    }
}

impl Widget for MonthGrid<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_month_grid(self.month, self.agenda_source, area, buf, self.styles);
    }
}

#[derive(Clone, Copy)]
pub struct AppView<'a> {
    app: &'a AppState,
    agenda_source: &'a dyn AgendaSource,
}

impl<'a> AppView<'a> {
    pub fn new(app: &'a AppState) -> Self {
        Self::with_agenda_source(app, &EMPTY_AGENDA_SOURCE)
    }

    pub fn with_agenda_source(app: &'a AppState, agenda_source: &'a dyn AgendaSource) -> Self {
        Self { app, agenda_source }
    }
}

impl Widget for AppView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        match (self.app.view_mode(), ResponsiveLayout::for_area(area)) {
            (ViewMode::Month, layout) if layout.should_render_month_grid() => {
                let month = self.app.calendar_month();
                MonthGrid::with_agenda_source(&month, self.agenda_source).render(area, buf);
            }
            (ViewMode::Month, layout) if layout.should_render_week_view() => {
                let month = self.app.calendar_month();
                WeekGrid::with_agenda_source(&month, self.agenda_source).render(area, buf);
            }
            (ViewMode::Month, _) => {
                DayView::responsive_fallback(self.app, self.agenda_source).render(area, buf)
            }
            (ViewMode::Day, _) => DayView::focused(self.app, self.agenda_source).render(area, buf),
        }

        if let Some(form) = self.app.create_form() {
            render_create_event_modal(form, area, buf, CreateModalStyles::new());
        }
    }
}

#[derive(Clone, Copy)]
pub struct DayView<'a> {
    app: &'a AppState,
    agenda_source: &'a dyn AgendaSource,
    context: DayViewContext,
    styles: DayViewStyles,
}

impl<'a> DayView<'a> {
    pub fn focused(app: &'a AppState, agenda_source: &'a dyn AgendaSource) -> Self {
        Self {
            app,
            agenda_source,
            context: DayViewContext::Focused,
            styles: DayViewStyles::new(),
        }
    }

    pub fn responsive_fallback(app: &'a AppState, agenda_source: &'a dyn AgendaSource) -> Self {
        Self {
            app,
            agenda_source,
            context: DayViewContext::ResponsiveFallback,
            styles: DayViewStyles::new(),
        }
    }
}

impl Widget for DayView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_day_view(
            self.app,
            self.agenda_source,
            self.context,
            area,
            buf,
            self.styles,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DayViewContext {
    Focused,
    ResponsiveFallback,
}

#[derive(Clone, Copy)]
pub struct WeekGrid<'a> {
    month: &'a CalendarMonth,
    agenda_source: &'a dyn AgendaSource,
    styles: MonthGridStyles,
}

impl<'a> WeekGrid<'a> {
    pub fn new(month: &'a CalendarMonth) -> Self {
        Self::with_agenda_source(month, &EMPTY_AGENDA_SOURCE)
    }

    pub fn with_agenda_source(
        month: &'a CalendarMonth,
        agenda_source: &'a dyn AgendaSource,
    ) -> Self {
        Self {
            month,
            agenda_source,
            styles: MonthGridStyles::new(),
        }
    }
}

impl Widget for WeekGrid<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        render_week_grid(self.month, self.agenda_source, area, buf, self.styles);
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

pub const DAY_VIEW_SPLIT_MIN_WIDTH: u16 = 72;
pub const DAY_VIEW_STACKED_MIN_WIDTH: u16 = 36;
pub const DAY_VIEW_STACKED_MIN_HEIGHT: u16 = 12;

const DAY_HEADER_HEIGHT: u16 = 2;
const DAY_PANEL_GAP: u16 = 1;
const MIN_DAY_PANEL_HEIGHT: u16 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayViewLayoutMode {
    Split,
    Stacked,
    Minimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayViewLayout {
    pub area: Rect,
    pub mode: DayViewLayoutMode,
    pub title_y: u16,
    pub summary_y: Option<u16>,
    pub agenda_area: Option<Rect>,
    pub timeline_area: Option<Rect>,
}

impl DayViewLayout {
    pub fn new(area: Rect) -> Self {
        if area.width == 0 || area.height == 0 {
            return Self::minimal(area);
        }

        let content_y = area.y.saturating_add(DAY_HEADER_HEIGHT);
        let content_height = area.height.saturating_sub(DAY_HEADER_HEIGHT);

        if area.width >= DAY_VIEW_SPLIT_MIN_WIDTH && content_height >= MIN_DAY_PANEL_HEIGHT {
            let timeline_width = area.width / 3;
            let agenda_width = area
                .width
                .saturating_sub(timeline_width)
                .saturating_sub(DAY_PANEL_GAP);

            return Self {
                area,
                mode: DayViewLayoutMode::Split,
                title_y: area.y,
                summary_y: Some(area.y + 1),
                agenda_area: Some(Rect::new(area.x, content_y, agenda_width, content_height)),
                timeline_area: Some(Rect::new(
                    area.x + agenda_width + DAY_PANEL_GAP,
                    content_y,
                    timeline_width,
                    content_height,
                )),
            };
        }

        if area.width >= DAY_VIEW_STACKED_MIN_WIDTH && area.height >= DAY_VIEW_STACKED_MIN_HEIGHT {
            let available = content_height.saturating_sub(DAY_PANEL_GAP);
            let agenda_height = available.saturating_mul(2) / 3;
            let timeline_height = available.saturating_sub(agenda_height);
            let timeline_y = content_y + agenda_height + DAY_PANEL_GAP;

            return Self {
                area,
                mode: DayViewLayoutMode::Stacked,
                title_y: area.y,
                summary_y: Some(area.y + 1),
                agenda_area: Some(Rect::new(area.x, content_y, area.width, agenda_height)),
                timeline_area: Some(Rect::new(area.x, timeline_y, area.width, timeline_height)),
            };
        }

        Self::minimal(area)
    }

    const fn minimal(area: Rect) -> Self {
        let summary_y = if area.height > 1 {
            Some(area.y + 1)
        } else {
            None
        };

        Self {
            area,
            mode: DayViewLayoutMode::Minimal,
            title_y: area.y,
            summary_y,
            agenda_area: None,
            timeline_area: None,
        }
    }
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
    preview: Style,
    preview_summary: Style,
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
            preview: Style::new().fg(Color::Gray),
            preview_summary: Style::new().fg(Color::Cyan).add_modifier(Modifier::DIM),
            filler: Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            filler_border: Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DayViewStyles {
    title: Style,
    summary: Style,
    panel_title: Style,
    border: Style,
    content: Style,
    muted: Style,
    timeline_mark: Style,
    timeline_event: Style,
    holiday: Style,
    event: Style,
}

impl DayViewStyles {
    const fn new() -> Self {
        Self {
            title: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            summary: Style::new().fg(Color::Gray),
            panel_title: Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
            border: Style::new().fg(Color::DarkGray),
            content: Style::new().fg(Color::White),
            muted: Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            timeline_mark: Style::new().fg(Color::Yellow).add_modifier(Modifier::DIM),
            timeline_event: Style::new().fg(Color::White).bg(Color::Blue),
            holiday: Style::new().fg(Color::Yellow),
            event: Style::new().fg(Color::White),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CreateModalStyles {
    panel: Style,
    border: Style,
    title: Style,
    label: Style,
    value: Style,
    error: Style,
    footer: Style,
}

impl CreateModalStyles {
    const fn new() -> Self {
        Self {
            panel: Style::new().fg(Color::White).bg(Color::Black),
            border: Style::new().fg(Color::Cyan).bg(Color::Black),
            title: Style::new()
                .fg(Color::Cyan)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
            label: Style::new().fg(Color::Gray).bg(Color::Black),
            value: Style::new().fg(Color::White).bg(Color::Black),
            error: Style::new()
                .fg(Color::Red)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
            footer: Style::new().fg(Color::DarkGray).bg(Color::Black),
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
    render_app_to_string_with_agenda_source(app, width, height, &EMPTY_AGENDA_SOURCE)
}

pub fn render_app_to_string_with_agenda_source<S>(
    app: &AppState,
    width: u16,
    height: u16,
    agenda_source: &S,
) -> String
where
    S: AgendaSource,
{
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    AppView::with_agenda_source(app, agenda_source).render(area, &mut buffer);
    buffer_to_string(&buffer)
}

pub fn hit_test_app_date(
    app: &AppState,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<CalendarDate> {
    if !contains_position(area, column, row) {
        return None;
    }

    let month = app.calendar_month();
    match (app.view_mode(), ResponsiveLayout::for_area(area)) {
        (ViewMode::Month, layout) if layout.should_render_month_grid() => {
            hit_test_month_grid_date(&month, area, column, row)
        }
        (ViewMode::Month, layout) if layout.should_render_week_view() => {
            hit_test_week_grid_date(&month, area, column, row)
        }
        _ => None,
    }
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

fn hit_test_month_grid_date(
    month: &CalendarMonth,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<CalendarDate> {
    let layout = MonthGridLayout::new(area)?;
    if !contains_position(layout.grid_area, column, row) {
        return None;
    }

    let week_index = hit_test_bounds(&layout.row_bounds, row)?;
    let weekday_index = hit_test_bounds(&layout.column_bounds, column)?;
    Some(month.weeks[week_index].cells[weekday_index].date)
}

fn hit_test_week_grid_date(
    month: &CalendarMonth,
    area: Rect,
    column: u16,
    row: u16,
) -> Option<CalendarDate> {
    let layout = WeekGridLayout::new(area)?;
    let week = selected_week(month)?;
    if !contains_position(layout.grid_area, column, row) {
        return None;
    }

    let weekday_index = hit_test_bounds(&layout.column_bounds, column)?;
    Some(week.cells[weekday_index].date)
}

fn hit_test_bounds(bounds: &[u16], coordinate: u16) -> Option<usize> {
    let final_index = bounds.len().checked_sub(2)?;

    for index in 0..=final_index {
        let start = bounds[index];
        let end = bounds[index + 1];
        if coordinate >= start && (coordinate < end || (index == final_index && coordinate == end))
        {
            return Some(index);
        }
    }

    None
}

fn contains_position(area: Rect, column: u16, row: u16) -> bool {
    area.contains(Position { x: column, y: row })
}

fn render_month_grid(
    month: &CalendarMonth,
    agenda_source: &dyn AgendaSource,
    area: Rect,
    buf: &mut Buffer,
    styles: MonthGridStyles,
) {
    let Some(layout) = MonthGridLayout::new(area) else {
        render_too_small_message(area, buf, styles);
        return;
    };

    buf.set_style(area, Style::default());
    render_title(month, &layout, buf, styles);
    render_weekdays(&layout, buf, styles);

    for cell in month
        .cells()
        .filter(|cell| !cell.is_selected && !cell.is_today)
    {
        render_cell(cell, &layout, agenda_source, buf, styles);
    }

    for cell in month
        .cells()
        .filter(|cell| cell.is_today && !cell.is_selected)
    {
        render_cell(cell, &layout, agenda_source, buf, styles);
    }

    if let Some(selected) = month.selected_cell() {
        render_cell(selected, &layout, agenda_source, buf, styles);
    }
}

fn render_week_grid(
    month: &CalendarMonth,
    agenda_source: &dyn AgendaSource,
    area: Rect,
    buf: &mut Buffer,
    styles: MonthGridStyles,
) {
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

    for cell in selected_week
        .cells
        .iter()
        .filter(|cell| !cell.is_selected && !cell.is_today)
    {
        render_week_cell(cell, &layout, agenda_source, buf, styles);
    }

    for cell in selected_week
        .cells
        .iter()
        .filter(|cell| cell.is_today && !cell.is_selected)
    {
        render_week_cell(cell, &layout, agenda_source, buf, styles);
    }

    if let Some(selected) = selected_week.cells.iter().find(|cell| cell.is_selected) {
        render_week_cell(selected, &layout, agenda_source, buf, styles);
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

fn render_day_view(
    app: &AppState,
    agenda_source: &dyn AgendaSource,
    context: DayViewContext,
    area: Rect,
    buf: &mut Buffer,
    styles: DayViewStyles,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let layout = DayViewLayout::new(area);
    let agenda = app.day_agenda(agenda_source);
    buf.set_style(area, Style::default());
    render_day_header(&agenda, context, &layout, buf, styles);

    match layout.mode {
        DayViewLayoutMode::Split | DayViewLayoutMode::Stacked => {
            if let Some(agenda_area) = layout.agenda_area {
                render_agenda_panel(&agenda, agenda_area, buf, styles);
            }

            if let Some(timeline_area) = layout.timeline_area {
                render_timeline_panel(&agenda, timeline_area, buf, styles);
            }
        }
        DayViewLayoutMode::Minimal => {
            if area.height > 2 {
                let text = if agenda.is_empty() {
                    "No agenda loaded".to_string()
                } else {
                    agenda_summary(&agenda)
                };
                write_centered(buf, area.y + 2, area.x, area.width, &text, styles.content);
            }
        }
    }
}

fn render_day_header(
    agenda: &DayAgenda,
    context: DayViewContext,
    layout: &DayViewLayout,
    buf: &mut Buffer,
    styles: DayViewStyles,
) {
    write_centered(
        buf,
        layout.title_y,
        layout.area.x,
        layout.area.width,
        &day_title(agenda.date),
        styles.title,
    );

    let Some(summary_y) = layout.summary_y else {
        return;
    };

    let summary = match context {
        DayViewContext::Focused => format!("{} | Esc returns to month", agenda_summary(agenda)),
        DayViewContext::ResponsiveFallback => agenda_summary(agenda),
    };
    write_centered(
        buf,
        summary_y,
        layout.area.x,
        layout.area.width,
        &summary,
        styles.summary,
    );
}

fn render_create_event_modal(
    form: &CreateEventForm,
    area: Rect,
    buf: &mut Buffer,
    styles: CreateModalStyles,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let modal = create_modal_area(area);
    fill_rect(buf, modal, styles.panel);
    draw_border(buf, modal, styles.border, BorderCharacters::normal());

    let content = inset_rect(modal);
    if content.width == 0 || content.height == 0 {
        return;
    }

    write_centered(
        buf,
        content.y,
        content.x,
        content.width,
        "Create",
        styles.title,
    );
    let label_width = 12.min(content.width.saturating_sub(1));

    for (y, row) in (content.y.saturating_add(2)..).zip(form.rows()) {
        if y >= content.bottom().saturating_sub(2) {
            break;
        }

        let marker = if row.focused { ">" } else { " " };
        write_padded_left(buf, y, content.x, 1, marker, styles.label);
        let label_x = content.x.saturating_add(2);
        write_padded_left(buf, y, label_x, label_width, row.label, styles.label);

        let value_x = label_x.saturating_add(label_width).saturating_add(1);
        if value_x < content.right() {
            let value_width = content.right() - value_x;
            write_padded_left(buf, y, value_x, value_width, &row.value, styles.value);
        }
    }

    if let Some(error) = form.error() {
        let error_y = content.bottom().saturating_sub(2);
        write_left(buf, error_y, content.x, content.width, error, styles.error);
    }

    let footer_y = content.bottom().saturating_sub(1);
    write_centered(
        buf,
        footer_y,
        content.x,
        content.width,
        "Tab fields | Ctrl-S save | Esc cancel",
        styles.footer,
    );
}

fn create_modal_area(area: Rect) -> Rect {
    if area.width < 52 || area.height < 16 {
        return area;
    }

    let width = area.width.saturating_sub(4).min(72);
    let height = area.height.saturating_sub(4).min(22);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn render_agenda_panel(agenda: &DayAgenda, area: Rect, buf: &mut Buffer, styles: DayViewStyles) {
    render_panel(area, "Agenda", buf, styles);

    let content = inset_rect(area);
    if content.width == 0 || content.height == 0 {
        return;
    }

    let mut y = content.y;
    write_left(
        buf,
        y,
        content.x,
        content.width,
        "Holidays",
        styles.panel_title,
    );
    y += 1;
    if y < content.bottom() {
        if agenda.holidays.is_empty() {
            write_left(buf, y, content.x, content.width, "None", styles.muted);
            y += 2;
        } else {
            for holiday in &agenda.holidays {
                if y >= content.bottom() {
                    return;
                }
                write_left(
                    buf,
                    y,
                    content.x,
                    content.width,
                    &format!("* {}", holiday.name),
                    styles.holiday,
                );
                y += 1;
            }
            y += 1;
        }
    }

    if !agenda.all_day_events.is_empty() && y < content.bottom() {
        write_left(
            buf,
            y,
            content.x,
            content.width,
            "All day",
            styles.panel_title,
        );
        y += 1;

        for event in &agenda.all_day_events {
            if y >= content.bottom() {
                return;
            }
            y = render_event_detail_lines(
                event,
                &format!("- {}", event.title),
                content,
                y,
                buf,
                styles,
            );
        }

        y += 1;
    }

    if y < content.bottom() {
        write_left(
            buf,
            y,
            content.x,
            content.width,
            "Events",
            styles.panel_title,
        );
        y += 1;
    }

    if y < content.bottom() {
        if agenda.timed_events.is_empty() {
            write_left(
                buf,
                y,
                content.x,
                content.width,
                "No events scheduled",
                styles.muted,
            );
        } else {
            for agenda_event in &agenda.timed_events {
                if y >= content.bottom() {
                    return;
                }
                y = render_event_detail_lines(
                    &agenda_event.event,
                    &agenda_event_line(agenda_event),
                    content,
                    y,
                    buf,
                    styles,
                );
            }
        }
    }
}

fn render_event_detail_lines(
    event: &Event,
    first_line: &str,
    content: Rect,
    mut y: u16,
    buf: &mut Buffer,
    styles: DayViewStyles,
) -> u16 {
    write_left(buf, y, content.x, content.width, first_line, styles.event);
    y += 1;

    if let Some(location) = &event.location {
        if y >= content.bottom() {
            return y;
        }
        write_left(
            buf,
            y,
            content.x,
            content.width,
            &format!("  @ {location}"),
            styles.muted,
        );
        y += 1;
    }

    if !event.reminders.is_empty() {
        if y >= content.bottom() {
            return y;
        }
        write_left(
            buf,
            y,
            content.x,
            content.width,
            &format!("  Reminders: {}", reminder_summary(event)),
            styles.muted,
        );
        y += 1;
    }

    if let Some(notes) = &event.notes {
        for line in notes.lines() {
            if y >= content.bottom() {
                return y;
            }
            write_left(
                buf,
                y,
                content.x,
                content.width,
                &format!("  {line}"),
                styles.muted,
            );
            y += 1;
        }
    }

    y
}

fn render_timeline_panel(agenda: &DayAgenda, area: Rect, buf: &mut Buffer, styles: DayViewStyles) {
    render_panel(area, "24-hour timeline", buf, styles);

    let content = inset_rect(area);
    if content.width == 0 || content.height == 0 {
        return;
    }

    if content.height >= 5 {
        let marks = ["00:00", "06:00", "12:00", "18:00", "24:00"];
        for (index, mark) in marks.into_iter().enumerate() {
            let offset = (u16::try_from(index).expect("timeline mark index fits in u16")
                * content.height.saturating_sub(1))
                / 4;
            write_left(
                buf,
                content.y + offset,
                content.x,
                content.width,
                mark,
                styles.timeline_mark,
            );
        }
    }

    if agenda.timed_events.is_empty() {
        let message_y = content.y + content.height / 2;
        write_centered(
            buf,
            message_y,
            content.x,
            content.width,
            "No timed events",
            styles.muted,
        );
        return;
    }

    render_timeline_events(&agenda.timed_events, content, buf, styles);
}

fn render_panel(area: Rect, title: &str, buf: &mut Buffer, styles: DayViewStyles) {
    draw_border(buf, area, styles.border, BorderCharacters::normal());

    if area.width <= 2 || area.height <= 2 {
        return;
    }

    write_left(
        buf,
        area.y,
        area.x + 2,
        area.width.saturating_sub(4),
        title,
        styles.panel_title,
    );
}

fn render_timeline_events(
    events: &[TimedAgendaEvent],
    content: Rect,
    buf: &mut Buffer,
    styles: DayViewStyles,
) {
    if content.width == 0 || content.height == 0 {
        return;
    }

    let event_x = content.x + content.width.min(7);
    if event_x >= content.right() {
        return;
    }

    let mut label_rows = Vec::new();

    for event in events {
        let start_y = timeline_y_for_minutes(content, event.visible_start.as_minutes());
        let end_minute = event.visible_end.as_minutes().saturating_sub(1);
        let end_y = timeline_y_for_minutes(content, end_minute);
        let group_offset = u16::try_from(event.overlap_group)
            .unwrap_or(u16::MAX)
            .saturating_mul(2);
        let block_x = event_x
            .saturating_add(group_offset)
            .min(content.right().saturating_sub(1));

        for y in start_y..=end_y.max(start_y) {
            set_cell(buf, block_x, y, "|", styles.timeline_event);
        }

        let label_x = block_x.saturating_add(2);
        if label_x < content.right() {
            let label_y = available_label_y(start_y, content, &mut label_rows);
            write_left(
                buf,
                label_y,
                label_x,
                content.right() - label_x,
                &agenda_event_line(event),
                styles.timeline_event,
            );
        }
    }
}

fn available_label_y(start_y: u16, content: Rect, used: &mut Vec<u16>) -> u16 {
    let mut y = start_y;
    while used.contains(&y) && y + 1 < content.bottom() {
        y += 1;
    }
    used.push(y);
    y
}

fn timeline_y_for_minutes(content: Rect, minute: u16) -> u16 {
    let clamped = u32::from(minute.min(DayMinute::END.as_minutes()));
    let height = u32::from(content.height.saturating_sub(1));
    let offset = (clamped * height) / u32::from(DayMinute::END.as_minutes());
    content.y + u16::try_from(offset).expect("timeline offset fits in terminal height")
}

fn agenda_summary(agenda: &DayAgenda) -> String {
    format!(
        "{} holidays | {} events",
        agenda.holidays.len(),
        agenda.all_day_events.len() + agenda.timed_events.len()
    )
}

fn agenda_event_line(event: &TimedAgendaEvent) -> String {
    let mut prefix = String::new();
    if event.starts_before_day {
        prefix.push('<');
    }

    prefix.push_str(&format!(
        "{}-{}",
        day_minute_label(event.visible_start),
        day_minute_label(event.visible_end)
    ));

    if event.ends_after_day {
        prefix.push('>');
    }

    format!("{prefix} {}", event.event.title)
}

fn reminder_summary(event: &Event) -> String {
    event
        .reminders
        .iter()
        .map(|reminder| reminder_label(reminder.minutes_before))
        .collect::<Vec<_>>()
        .join(", ")
}

fn reminder_label(minutes: u16) -> String {
    match minutes {
        value if value % (24 * 60) == 0 => format!("{}d", value / (24 * 60)),
        value if value % 60 == 0 => format!("{}h", value / 60),
        value => format!("{value}m"),
    }
}

fn month_preview_labels(agenda: &DayAgenda) -> Vec<String> {
    agenda
        .holidays
        .iter()
        .map(|holiday| holiday.name.clone())
        .chain(
            agenda
                .all_day_events
                .iter()
                .map(|event| event.title.clone()),
        )
        .chain(
            agenda
                .timed_events
                .iter()
                .map(|event| month_event_preview_label(&event.event)),
        )
        .collect()
}

fn month_event_preview_label(event: &Event) -> String {
    match event.timing {
        EventTiming::Timed { start, .. } => {
            format!(
                "{} {}",
                day_minute_label(DayMinute::from_time(start.time)),
                event.title
            )
        }
        EventTiming::AllDay { .. } => event.title.clone(),
    }
}

fn month_preview_summary(count: usize, width: u16) -> String {
    if width >= 9 {
        format!("+{count} Events")
    } else {
        format!("+{count}")
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
    agenda_source: &dyn AgendaSource,
    buf: &mut Buffer,
    styles: MonthGridStyles,
) {
    let rect = layout.cell_rect(cell.week_index, cell.weekday_index);
    let content = layout.cell_content_rect(cell.week_index, cell.weekday_index);
    render_cell_in_rect(cell, rect, content, agenda_source, buf, styles);
}

fn render_week_cell(
    cell: &CalendarCell,
    layout: &WeekGridLayout,
    agenda_source: &dyn AgendaSource,
    buf: &mut Buffer,
    styles: MonthGridStyles,
) {
    let rect = layout.cell_rect(cell.weekday_index);
    let content = layout.cell_content_rect(cell.weekday_index);
    render_cell_in_rect(cell, rect, content, agenda_source, buf, styles);
}

fn render_cell_in_rect(
    cell: &CalendarCell,
    rect: Rect,
    content: Rect,
    agenda_source: &dyn AgendaSource,
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
    render_cell_previews(cell, content, agenda_source, buf, styles);
}

fn render_cell_previews(
    cell: &CalendarCell,
    content: Rect,
    agenda_source: &dyn AgendaSource,
    buf: &mut Buffer,
    styles: MonthGridStyles,
) {
    if !cell.is_in_visible_month || content.width == 0 || content.height <= 1 {
        return;
    }

    let agenda = DayAgenda::from_source(cell.date, agenda_source);
    let previews = month_preview_labels(&agenda);
    if previews.is_empty() {
        return;
    }

    let capacity = usize::from(content.height - 1);
    let first_y = content.y + 1;
    if previews.len() > capacity {
        let summary = month_preview_summary(previews.len(), content.width);
        write_left(
            buf,
            first_y,
            content.x,
            content.width,
            &summary,
            styles.preview_summary,
        );
        return;
    }

    for (index, preview) in previews.into_iter().enumerate() {
        write_left(
            buf,
            first_y + u16::try_from(index).expect("preview index fits in u16"),
            content.x,
            content.width,
            &preview,
            styles.preview,
        );
    }
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

fn day_title(date: CalendarDate) -> String {
    format!(
        "{}, {} {}, {}",
        date.weekday(),
        date.month(),
        date.day(),
        date.year()
    )
}

fn day_minute_label(minute: DayMinute) -> String {
    let minutes = minute.as_minutes();
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
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
        cell.reset();
        cell.set_symbol(symbol).set_style(style);
    }
}

fn fill_rect(buf: &mut Buffer, rect: Rect, style: Style) {
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            set_cell(buf, x, y, " ", style);
        }
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

fn write_left(buf: &mut Buffer, y: u16, x: u16, width: u16, text: &str, style: Style) {
    if width == 0 || !buf.area.contains((x, y).into()) {
        return;
    }

    buf.set_stringn(x, y, text, usize::from(width), style);
}

fn write_padded_left(buf: &mut Buffer, y: u16, x: u16, width: u16, text: &str, style: Style) {
    if width == 0 || !buf.area.contains((x, y).into()) {
        return;
    }

    for column in x..x.saturating_add(width) {
        set_cell(buf, column, y, " ", style);
    }
    buf.set_stringn(x, y, text, usize::from(width), style);
}

const fn inset_rect(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
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
    use time::{Month, Time};

    use crate::{
        agenda::{Event, EventDateTime, Holiday, InMemoryAgendaSource, Reminder, SourceMetadata},
        app::AppAction,
        calendar::CalendarDate,
    };

    #[derive(Debug, Clone, Copy)]
    struct ExpectedBorder<'a> {
        horizontal: &'a str,
        vertical: &'a str,
        corner: &'a str,
        fg: Color,
        bg: Color,
        modifier: Modifier,
    }

    fn date(year: i32, month: Month, day: u8) -> CalendarDate {
        CalendarDate::from_ymd(year, month, day).expect("valid test date")
    }

    fn render_test_buffer(month: &CalendarMonth, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        MonthGrid::new(month).render(area, &mut buffer);
        buffer
    }

    fn render_app_buffer(app: &AppState, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        AppView::new(app).render(area, &mut buffer);
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

    fn assert_cell_perimeter(buffer: &Buffer, rect: Rect, expected: ExpectedBorder<'_>) {
        let left = rect.left();
        let right = rect.right().saturating_sub(1);
        let top = rect.top();
        let bottom = rect.bottom().saturating_sub(1);
        let border_points = [
            ((left, top), expected.corner),
            ((right, top), expected.corner),
            ((left, bottom), expected.corner),
            ((right, bottom), expected.corner),
            ((left + 1, top), expected.horizontal),
            ((left + 1, bottom), expected.horizontal),
            ((left, top + 1), expected.vertical),
            ((right, top + 1), expected.vertical),
        ];

        for (position, symbol) in border_points {
            let cell = buffer.cell(position).expect("border cell exists");
            assert_eq!(cell.symbol(), symbol);
            assert_eq!(cell.fg, expected.fg);
            assert_eq!(cell.bg, expected.bg);
            assert!(cell.modifier.contains(expected.modifier));
        }
    }

    fn assert_styled_text(buffer: &Buffer, x: u16, y: u16, text: &str, fg: Color) {
        for (offset, character) in text.chars().enumerate() {
            let cell = buffer
                .cell((x + u16::try_from(offset).expect("offset fits"), y))
                .expect("text cell exists");
            assert_eq!(cell.symbol(), character.to_string());
            assert_eq!(cell.fg, fg);
            assert_eq!(cell.bg, Color::Black);
            assert!(!cell.modifier.contains(Modifier::DIM));
            assert!(!cell.modifier.contains(Modifier::BOLD));
        }
    }

    fn centered(width: usize, text: &str) -> String {
        let padding = width.saturating_sub(text.len());
        let left = padding / 2;
        let right = padding - left;
        format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
    }

    fn source_metadata() -> SourceMetadata {
        SourceMetadata::fixture()
    }

    fn at(date: CalendarDate, hour: u8, minute: u8) -> EventDateTime {
        EventDateTime::new(
            date,
            Time::from_hms(hour, minute, 0).expect("valid test time"),
        )
    }

    fn timed_event(id: &str, title: &str, start: EventDateTime, end: EventDateTime) -> Event {
        Event::timed(id, title, start, end, source_metadata()).expect("valid test event")
    }

    fn agenda_source(events: Vec<Event>, holidays: Vec<Holiday>) -> InMemoryAgendaSource {
        InMemoryAgendaSource::with_events_and_holidays(events, holidays)
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

        let today_rect = layout.cell_rect(today.week_index, today.weekday_index);
        assert_cell_perimeter(
            &buffer,
            today_rect,
            ExpectedBorder {
                horizontal: "=",
                vertical: "!",
                corner: "+",
                fg: Color::Yellow,
                bg: Color::Reset,
                modifier: Modifier::DIM,
            },
        );

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

        assert_cell_perimeter(
            &buffer,
            rect,
            ExpectedBorder {
                horizontal: "#",
                vertical: "#",
                corner: "#",
                fg: Color::Cyan,
                bg: Color::Blue,
                modifier: Modifier::BOLD,
            },
        );
    }

    #[test]
    fn today_week_cell_has_full_focus_perimeter() {
        let month =
            CalendarMonth::from_dates(date(2026, Month::April, 20), date(2026, Month::April, 23));
        let area = Rect::new(0, 0, 84, 8);
        let mut buffer = Buffer::empty(area);
        WeekGrid::new(&month).render(area, &mut buffer);

        let layout = WeekGridLayout::new(area).expect("supported week layout");
        let week = selected_week(&month).expect("selected week exists");
        let today = week
            .cells
            .iter()
            .find(|cell| cell.is_today)
            .expect("today cell appears in selected week");
        let rect = layout.cell_rect(today.weekday_index);

        assert_cell_perimeter(
            &buffer,
            rect,
            ExpectedBorder {
                horizontal: "=",
                vertical: "!",
                corner: "+",
                fg: Color::Yellow,
                bg: Color::Reset,
                modifier: Modifier::DIM,
            },
        );
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
    fn create_modal_renders_over_month_view() {
        let mut app = AppState::new(date(2026, Month::April, 23));
        app.apply(AppAction::OpenCreate);

        let rendered = render_app_to_string(&app, 84, 26);

        assert!(rendered.contains("Create"));
        assert!(rendered.contains("Title"));
        assert!(rendered.contains("Start date"));
        assert!(rendered.contains("Reminder"));
        assert!(rendered.contains("Ctrl-S save"));
    }

    #[test]
    fn create_modal_clears_background_content() {
        let selected = date(2026, Month::April, 23);
        let mut app = AppState::new(selected);
        app.apply(AppAction::OpenCreate);
        let source = agenda_source(
            vec![timed_event(
                "behind",
                "BackdropGhost",
                at(selected, 9, 0),
                at(selected, 10, 0),
            )],
            Vec::new(),
        );

        let rendered = render_app_to_string_with_agenda_source(&app, 84, 26, &source);

        assert!(rendered.contains("Create"));
        assert!(!rendered.contains("Backdrop"));
        assert!(!rendered.contains("Ghost"));
    }

    #[test]
    fn create_modal_uses_gray_labels_and_white_values() {
        let selected = date(2026, Month::April, 23);
        let mut app = AppState::new(selected);
        app.apply(AppAction::OpenCreate);

        let area = Rect::new(0, 0, 84, 26);
        let buffer = render_app_buffer(&app, area.width, area.height);
        let modal = create_modal_area(area);
        let content = inset_rect(modal);
        let row_y = content.y.saturating_add(2);
        let label_x = content.x.saturating_add(2);
        let label_width = 12.min(content.width.saturating_sub(1));
        let value_x = label_x.saturating_add(label_width).saturating_add(1);

        assert_styled_text(&buffer, content.x, row_y, ">", Color::Gray);
        assert_styled_text(&buffer, label_x, row_y, "Title", Color::Gray);
        assert_styled_text(&buffer, label_x, row_y + 2, "Start date", Color::Gray);
        assert_styled_text(&buffer, value_x, row_y + 1, "[ ]", Color::White);
        assert_styled_text(&buffer, value_x, row_y + 2, "2026-04-23", Color::White);
        assert_styled_text(&buffer, value_x, row_y + 3, "09:00", Color::White);
        assert_styled_text(&buffer, value_x, row_y + 8, "[ ] 5m", Color::White);
    }

    #[test]
    fn create_modal_uses_full_screen_area_when_tight() {
        let mut app = AppState::new(date(2026, Month::April, 23));
        app.apply(AppAction::OpenCreate);

        let rendered = render_app_to_string(&app, 40, 10);
        let lines = rendered.lines().collect::<Vec<_>>();

        assert_eq!(lines.len(), 10);
        assert!(lines[1].contains("Create"));
        assert!(rendered.contains("Title"));
        assert!(rendered.contains("Ctrl-S save"));
    }

    #[test]
    fn hit_test_selects_date_in_month_grid() {
        let app = AppState::new(date(2026, Month::April, 23));
        let area = Rect::new(0, 0, 84, 26);
        let month = app.calendar_month();
        let layout = MonthGridLayout::new(area).expect("supported layout");
        let target = cell_for_day(&month, 18);
        let content = layout.cell_content_rect(target.week_index, target.weekday_index);

        assert_eq!(
            hit_test_app_date(&app, area, content.x, content.y),
            Some(date(2026, Month::April, 18))
        );
        assert_eq!(
            hit_test_app_date(
                &app,
                area,
                layout.cell_rect(target.week_index, target.weekday_index).x,
                layout.cell_rect(target.week_index, target.weekday_index).y,
            ),
            Some(date(2026, Month::April, 18))
        );
    }

    #[test]
    fn hit_test_ignores_month_grid_chrome() {
        let app = AppState::new(date(2026, Month::April, 23));
        let area = Rect::new(0, 0, 84, 26);

        assert_eq!(hit_test_app_date(&app, area, 0, 0), None);
        assert_eq!(hit_test_app_date(&app, area, 83, 1), None);
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
    fn hit_test_selects_date_in_week_fallback() {
        let app = AppState::new(date(2026, Month::April, 23));
        let area = Rect::new(0, 0, 84, 8);
        let month = app.calendar_month();
        let week = selected_week(&month).expect("selected week exists");
        let target = week
            .cells
            .iter()
            .find(|cell| cell.date.day() == 21)
            .expect("target date appears in selected week");
        let layout = WeekGridLayout::new(area).expect("supported week layout");
        let content = layout.cell_content_rect(target.weekday_index);

        assert_eq!(
            hit_test_app_date(&app, area, content.x, content.y),
            Some(date(2026, Month::April, 21))
        );
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
    fn hit_test_ignores_day_fallback_and_focused_day_view() {
        let month_app = AppState::new(date(2026, Month::April, 23));
        assert_eq!(
            hit_test_app_date(&month_app, Rect::new(0, 0, 35, 10), 10, 2),
            None
        );

        let mut day_app = AppState::new(date(2026, Month::April, 23));
        day_app.apply(AppAction::OpenDay);
        assert_eq!(
            hit_test_app_date(&day_app, Rect::new(0, 0, 84, 14), 10, 2),
            None
        );
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
    fn day_view_layout_splits_when_width_allows() {
        let layout = DayViewLayout::new(Rect::new(0, 0, 84, 14));

        assert_eq!(layout.mode, DayViewLayoutMode::Split);
        assert_eq!(layout.agenda_area.expect("agenda area").width, 55);
        assert_eq!(layout.timeline_area.expect("timeline area").width, 28);
    }

    #[test]
    fn day_view_layout_stacks_when_width_is_constrained() {
        let layout = DayViewLayout::new(Rect::new(0, 0, 49, 14));

        assert_eq!(layout.mode, DayViewLayoutMode::Stacked);
        assert!(layout.agenda_area.expect("agenda area").height > 0);
        assert!(layout.timeline_area.expect("timeline area").height > 0);
    }

    #[test]
    fn day_view_layout_simplifies_when_panels_cannot_fit() {
        let layout = DayViewLayout::new(Rect::new(0, 0, 35, 10));

        assert_eq!(layout.mode, DayViewLayoutMode::Minimal);
        assert!(layout.agenda_area.is_none());
        assert!(layout.timeline_area.is_none());
    }

    #[test]
    fn app_view_renders_focused_empty_day_shell() {
        let app = {
            let mut app = AppState::new(date(2026, Month::April, 23));
            app.apply(AppAction::OpenDay);
            app
        };
        let rendered = render_app_to_string(&app, 84, 14);
        let lines: Vec<_> = rendered.lines().collect();

        assert_eq!(lines[0], centered(84, "Thursday, April 23, 2026"));
        assert_eq!(
            lines[1],
            centered(84, "0 holidays | 0 events | Esc returns to month")
        );
        assert!(rendered.contains("Agenda"));
        assert!(rendered.contains("Holidays"));
        assert!(rendered.contains("No events scheduled"));
        assert!(rendered.contains("24-hour timeline"));
        assert!(rendered.contains("No timed events"));
        assert!(!rendered.contains("April 2026"));
    }

    #[test]
    fn day_view_render_follows_arrow_selected_date() {
        let app = {
            let mut app = AppState::new(date(2026, Month::April, 23));
            app.apply(AppAction::OpenDay);
            app.apply(AppAction::MoveDays(-1));
            app
        };

        let rendered = render_app_to_string(&app, 84, 14);

        assert!(rendered.contains("Wednesday, April 22, 2026"));
        assert!(!rendered.contains("Thursday, April 23, 2026"));
    }

    #[test]
    fn day_view_renders_holidays_events_and_timeline_blocks() {
        let app = {
            let mut app = AppState::new(date(2026, Month::April, 23));
            app.apply(AppAction::OpenDay);
            app
        };
        let mut source = InMemoryAgendaSource::development_fixture();
        source.push_holiday(Holiday::new(
            "earth-day",
            "Earth Day",
            date(2026, Month::April, 23),
            source_metadata(),
        ));
        let rendered = render_app_to_string_with_agenda_source(&app, 84, 16, &source);

        assert!(rendered.contains("1 holidays | 4 events | Esc returns to month"));
        assert!(rendered.contains("* Earth Day"));
        assert!(rendered.contains("- Release day"));
        assert!(rendered.contains("09:00-09:30 Standup"));
        assert!(rendered.contains("09:15-10:00 Review"));
        assert!(rendered.contains("23:00-24:00> Late deploy"));
        assert!(!rendered.contains("No timed events"));
    }

    #[test]
    fn day_view_renders_event_details_when_space_allows() {
        let day = date(2026, Month::April, 23);
        let app = {
            let mut app = AppState::new(day);
            app.apply(AppAction::OpenDay);
            app
        };
        let event = timed_event("planning", "Planning", at(day, 9, 0), at(day, 10, 0))
            .with_location("War room")
            .with_notes("Bring notes")
            .with_reminders(vec![
                Reminder::minutes_before(10),
                Reminder::minutes_before(60),
            ]);
        let source = agenda_source(vec![event], Vec::new());

        let rendered = render_app_to_string_with_agenda_source(&app, 84, 18, &source);

        assert!(rendered.contains("09:00-10:00 Planning"));
        assert!(rendered.contains("@ War room"));
        assert!(rendered.contains("Reminders: 10m, 1h"));
        assert!(rendered.contains("Bring notes"));
    }

    #[test]
    fn overlapping_timeline_events_keep_distinct_labels() {
        let day = date(2026, Month::April, 23);
        let app = {
            let mut app = AppState::new(day);
            app.apply(AppAction::OpenDay);
            app
        };
        let source = agenda_source(
            vec![
                timed_event("alpha", "Alpha", at(day, 9, 0), at(day, 10, 0)),
                timed_event("beta", "Beta", at(day, 9, 30), at(day, 10, 30)),
            ],
            Vec::new(),
        );
        let rendered = render_app_to_string_with_agenda_source(&app, 84, 14, &source);
        let lines = rendered.lines().collect::<Vec<_>>();
        let alpha_line = lines
            .iter()
            .position(|line| line.contains("09:00-10:00 Alpha"))
            .expect("alpha appears on timeline");
        let beta_line = lines
            .iter()
            .position(|line| line.contains("09:30-10:30 Beta"))
            .expect("beta appears on timeline");

        assert_ne!(alpha_line, beta_line);
        assert!(rendered.contains("0 holidays | 2 events | Esc returns to month"));
    }

    #[test]
    fn month_cells_render_previews_when_space_allows() {
        let day = date(2026, Month::April, 23);
        let app = AppState::new(day);
        let source = agenda_source(
            vec![timed_event("call", "Call", at(day, 9, 0), at(day, 9, 30))],
            vec![Holiday::new("holiday", "Holiday", day, source_metadata())],
        );
        let rendered = render_app_to_string_with_agenda_source(&app, 84, 26, &source);

        assert!(rendered.contains("Holiday"));
        assert!(rendered.contains("09:00 Call"));
        assert!(!rendered.contains("+2 Events"));
    }

    #[test]
    fn month_cells_use_compact_summary_when_event_count_exceeds_space() {
        let day = date(2026, Month::April, 23);
        let app = AppState::new(day);
        let source = agenda_source(
            vec![
                timed_event("one", "One", at(day, 9, 0), at(day, 9, 30)),
                timed_event("two", "Two", at(day, 10, 0), at(day, 10, 30)),
                timed_event("three", "Three", at(day, 11, 0), at(day, 11, 30)),
            ],
            Vec::new(),
        );
        let rendered = render_app_to_string_with_agenda_source(&app, 84, 26, &source);

        assert!(rendered.contains("+3 Events"));
        assert!(!rendered.contains("09:00 One"));
    }

    #[test]
    fn responsive_day_fallback_omits_close_hint() {
        let app = AppState::new(date(2026, Month::April, 23));
        let rendered = render_app_to_string(&app, 35, 10);

        assert!(rendered.contains("April 23, 2026"));
        assert!(rendered.contains("No agenda loaded"));
        assert!(rendered.contains("0 holidays | 0 events"));
        assert!(!rendered.contains("Esc returns to month"));
    }
}
