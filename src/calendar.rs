use std::{array, fmt};

use time::{Date, Month, Weekday};

pub const DAYS_PER_WEEK: usize = 7;
pub const MONTH_GRID_WEEKS: usize = 6;
pub const MONTH_GRID_CELLS: usize = DAYS_PER_WEEK * MONTH_GRID_WEEKS;

pub const SUNDAY_FIRST_WEEKDAYS: [Weekday; DAYS_PER_WEEK] = [
    Weekday::Sunday,
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
    Weekday::Saturday,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CalendarDate(Date);

impl CalendarDate {
    pub const fn new(date: Date) -> Self {
        Self(date)
    }

    pub fn from_ymd(year: i32, month: Month, day: u8) -> Result<Self, time::error::ComponentRange> {
        Date::from_calendar_date(year, month, day).map(Self)
    }

    pub const fn inner(self) -> Date {
        self.0
    }

    pub const fn year(self) -> i32 {
        self.0.year()
    }

    pub const fn month(self) -> Month {
        self.0.month()
    }

    pub const fn day(self) -> u8 {
        self.0.day()
    }

    pub const fn weekday(self) -> Weekday {
        self.0.weekday()
    }

    pub fn add_days(self, days: i32) -> Self {
        let mut date = self.0;

        if days >= 0 {
            for _ in 0..days {
                date = date
                    .next_day()
                    .expect("calendar navigation requires a representable next day");
            }
        } else {
            for _ in 0..days.saturating_abs() {
                date = date
                    .previous_day()
                    .expect("calendar navigation requires a representable previous day");
            }
        }

        Self(date)
    }
}

impl From<Date> for CalendarDate {
    fn from(value: Date) -> Self {
        Self(value)
    }
}

impl From<CalendarDate> for Date {
    fn from(value: CalendarDate) -> Self {
        value.0
    }
}

impl fmt::Display for CalendarDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}",
            self.year(),
            u8::from(self.month()),
            self.day()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonthId {
    pub year: i32,
    pub month: Month,
}

impl MonthId {
    pub const fn new(year: i32, month: Month) -> Self {
        Self { year, month }
    }

    pub fn from_date(date: CalendarDate) -> Self {
        Self {
            year: date.year(),
            month: date.month(),
        }
    }

    pub fn first_day(self) -> CalendarDate {
        CalendarDate::from_ymd(self.year, self.month, 1).expect("month metadata is valid")
    }

    pub const fn length(self) -> u8 {
        self.month.length(self.year)
    }

    pub const fn previous(self) -> Self {
        let month = self.month.previous();
        let year = if matches!(self.month, Month::January) {
            self.year - 1
        } else {
            self.year
        };

        Self { year, month }
    }

    pub const fn next(self) -> Self {
        let month = self.month.next();
        let year = if matches!(self.month, Month::December) {
            self.year + 1
        } else {
            self.year
        };

        Self { year, month }
    }

    pub fn date(self, day: u8) -> Option<CalendarDate> {
        CalendarDate::from_ymd(self.year, self.month, day).ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub selected_date: CalendarDate,
}

impl Selection {
    pub const fn new(selected_date: CalendarDate) -> Self {
        Self { selected_date }
    }

    pub const fn from_launch_date(start_date: CalendarDate) -> Self {
        Self::new(start_date)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarCell {
    pub date: CalendarDate,
    pub weekday: Weekday,
    pub week_index: usize,
    pub weekday_index: usize,
    pub is_in_visible_month: bool,
    pub is_today: bool,
    pub is_selected: bool,
}

impl CalendarCell {
    pub const fn is_filler(self) -> bool {
        !self.is_in_visible_month
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarWeek {
    pub index: usize,
    pub cells: [CalendarCell; DAYS_PER_WEEK],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarMonth {
    pub current: MonthId,
    pub previous: MonthId,
    pub next: MonthId,
    pub month_length: u8,
    pub week_start: Weekday,
    pub weekdays: [Weekday; DAYS_PER_WEEK],
    pub selection: Selection,
    pub today: CalendarDate,
    pub weeks: [CalendarWeek; MONTH_GRID_WEEKS],
}

impl CalendarMonth {
    pub fn for_launch_date(start_date: CalendarDate) -> Self {
        Self::from_selection(Selection::from_launch_date(start_date), start_date)
    }

    pub fn from_dates(selected_date: CalendarDate, today: CalendarDate) -> Self {
        Self::from_selection(Selection::new(selected_date), today)
    }

    pub fn from_selection(selection: Selection, today: CalendarDate) -> Self {
        let current = MonthId::from_date(selection.selected_date);
        let month_length = current.length();
        let first_visible_date = first_visible_date(current.first_day());
        let weeks = build_weeks(first_visible_date, current, selection, today);

        Self {
            current,
            previous: current.previous(),
            next: current.next(),
            month_length,
            week_start: Weekday::Sunday,
            weekdays: SUNDAY_FIRST_WEEKDAYS,
            selection,
            today,
            weeks,
        }
    }

    pub fn cells(&self) -> impl Iterator<Item = &CalendarCell> {
        self.weeks.iter().flat_map(|week| week.cells.iter())
    }

    pub fn selected_cell(&self) -> Option<&CalendarCell> {
        self.cells().find(|cell| cell.is_selected)
    }

    pub fn today_cell(&self) -> Option<&CalendarCell> {
        self.cells().find(|cell| cell.is_today)
    }
}

fn build_weeks(
    first_visible_date: CalendarDate,
    current: MonthId,
    selection: Selection,
    today: CalendarDate,
) -> [CalendarWeek; MONTH_GRID_WEEKS] {
    array::from_fn(|week_index| CalendarWeek {
        index: week_index,
        cells: array::from_fn(|weekday_index| {
            let day_offset = week_index * DAYS_PER_WEEK + weekday_index;
            let date = add_days(first_visible_date, day_offset);

            CalendarCell {
                date,
                weekday: SUNDAY_FIRST_WEEKDAYS[weekday_index],
                week_index,
                weekday_index,
                is_in_visible_month: MonthId::from_date(date) == current,
                is_today: date == today,
                is_selected: date == selection.selected_date,
            }
        }),
    })
}

fn first_visible_date(first_of_month: CalendarDate) -> CalendarDate {
    let mut date = first_of_month.inner();

    for _ in 0..first_of_month.weekday().number_days_from_sunday() {
        date = date
            .previous_day()
            .expect("calendar grid requires a representable previous day");
    }

    CalendarDate::from(date)
}

fn add_days(start: CalendarDate, days: usize) -> CalendarDate {
    let mut date = start.inner();

    for _ in 0..days {
        date = date
            .next_day()
            .expect("calendar grid requires a representable next day");
    }

    CalendarDate::from(date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Month::{
        April, August, December, February, January, June, March, May, November, October, September,
    };

    fn date(year: i32, month: Month, day: u8) -> CalendarDate {
        CalendarDate::from_ymd(year, month, day).expect("valid test date")
    }

    #[test]
    fn month_lengths_include_leap_year_february() {
        let leap = CalendarMonth::for_launch_date(date(2024, February, 10));
        let common = CalendarMonth::for_launch_date(date(2026, February, 10));

        assert_eq!(leap.month_length, 29);
        assert_eq!(common.month_length, 28);
    }

    #[test]
    fn grid_has_stable_six_week_sunday_first_shape() {
        let month = CalendarMonth::for_launch_date(date(2026, April, 23));

        assert_eq!(month.week_start, Weekday::Sunday);
        assert_eq!(month.weekdays, SUNDAY_FIRST_WEEKDAYS);
        assert_eq!(month.weeks.len(), MONTH_GRID_WEEKS);
        assert_eq!(month.cells().count(), MONTH_GRID_CELLS);
        assert!(
            month
                .weeks
                .iter()
                .all(|week| week.cells.len() == DAYS_PER_WEEK)
        );
    }

    #[test]
    fn months_beginning_on_each_weekday_have_matching_padding() {
        let examples = [
            (date(2024, September, 1), Weekday::Sunday),
            (date(2024, April, 1), Weekday::Monday),
            (date(2024, October, 1), Weekday::Tuesday),
            (date(2024, May, 1), Weekday::Wednesday),
            (date(2024, August, 1), Weekday::Thursday),
            (date(2024, November, 1), Weekday::Friday),
            (date(2024, June, 1), Weekday::Saturday),
        ];

        for (first_day, weekday) in examples {
            let month = CalendarMonth::for_launch_date(first_day);
            let first_in_month = month
                .cells()
                .find(|cell| cell.date == first_day)
                .expect("first day appears in its month grid");

            assert_eq!(first_in_month.weekday, weekday);
            assert_eq!(
                first_in_month.weekday_index,
                usize::from(weekday.number_days_from_sunday())
            );
        }
    }

    #[test]
    fn filler_cells_include_previous_and_next_month_dates() {
        let month = CalendarMonth::for_launch_date(date(2026, April, 23));

        let first = month.weeks[0].cells[0];
        let last = month.weeks[MONTH_GRID_WEEKS - 1].cells[DAYS_PER_WEEK - 1];

        assert_eq!(first.date, date(2026, March, 29));
        assert!(first.is_filler());
        assert_eq!(last.date, date(2026, May, 9));
        assert!(last.is_filler());
    }

    #[test]
    fn selection_initializes_from_launch_date() {
        let start_date = date(2026, April, 23);
        let month = CalendarMonth::for_launch_date(start_date);

        assert_eq!(
            month.selection,
            Selection {
                selected_date: start_date
            }
        );
        assert_eq!(
            month.selected_cell().map(|cell| cell.date),
            Some(start_date)
        );
        assert_eq!(month.today_cell().map(|cell| cell.date), Some(start_date));
    }

    #[test]
    fn selected_date_and_today_can_be_marked_separately() {
        let selected = date(2026, April, 18);
        let today = date(2026, April, 23);
        let month = CalendarMonth::from_dates(selected, today);

        let selected_cell = month.selected_cell().expect("selected cell exists");
        let today_cell = month.today_cell().expect("today cell exists");

        assert_eq!(selected_cell.date, selected);
        assert!(selected_cell.is_selected);
        assert!(!selected_cell.is_today);

        assert_eq!(today_cell.date, today);
        assert!(today_cell.is_today);
        assert!(!today_cell.is_selected);
    }

    #[test]
    fn previous_and_next_metadata_cross_year_boundaries() {
        let january = CalendarMonth::for_launch_date(date(2026, January, 1));
        let december = CalendarMonth::for_launch_date(date(2026, December, 1));

        assert_eq!(january.previous, MonthId::new(2025, December));
        assert_eq!(january.next, MonthId::new(2026, February));

        assert_eq!(december.previous, MonthId::new(2026, November));
        assert_eq!(december.next, MonthId::new(2027, January));
    }

    #[test]
    fn visible_month_contains_exactly_its_own_days() {
        let month = CalendarMonth::for_launch_date(date(2026, February, 10));

        let visible_days = month
            .cells()
            .filter(|cell| cell.is_in_visible_month)
            .map(|cell| cell.date.day())
            .collect::<Vec<_>>();

        assert_eq!(visible_days.len(), usize::from(month.month_length));
        assert_eq!(visible_days.first(), Some(&1));
        assert_eq!(visible_days.last(), Some(&28));
    }
}
