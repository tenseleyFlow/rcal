use ratatui::layout::Rect;

/// Minimum terminal width where each month grid column can keep a readable
/// bordered day cell.
pub const MIN_MONTH_GRID_WIDTH: u16 = 49;
/// Minimum terminal height where the title, weekdays, and six week rows stay
/// visible without collapsing the grid into label-only cells.
pub const MIN_MONTH_GRID_HEIGHT: u16 = 20;
/// Minimum terminal width where a selected week can still show all seven day
/// cells with readable day labels.
pub const MIN_WEEK_VIEW_WIDTH: u16 = 36;
/// Minimum terminal height where a selected week can show a title, weekday
/// labels, and one bordered row of day cells.
pub const MIN_WEEK_VIEW_HEIGHT: u16 = 6;
/// A terminal is treated as portrait when the width is less than twice the
/// height. Portrait shape does not block month view when the month thresholds
/// fit, but it explains why narrower terminals fall back to selected week view.
pub const PORTRAIT_WIDTH_TO_HEIGHT_RATIO: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsiveMode {
    MonthGrid,
    WeekView,
    DayFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutConstraint {
    FitsMonthGrid,
    Portrait,
    MonthTooNarrow,
    MonthTooShort,
    MonthTooNarrowAndShort,
    WeekTooNarrow,
    WeekTooShort,
    WeekTooNarrowAndShort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResponsiveLayout {
    pub mode: ResponsiveMode,
    pub constraint: LayoutConstraint,
}

impl ResponsiveLayout {
    pub const fn for_area(area: Rect) -> Self {
        if area.width >= MIN_MONTH_GRID_WIDTH && area.height >= MIN_MONTH_GRID_HEIGHT {
            return Self {
                mode: ResponsiveMode::MonthGrid,
                constraint: LayoutConstraint::FitsMonthGrid,
            };
        }

        let month_constraint = month_constraint(area);
        if area.width >= MIN_WEEK_VIEW_WIDTH && area.height >= MIN_WEEK_VIEW_HEIGHT {
            return Self {
                mode: ResponsiveMode::WeekView,
                constraint: month_constraint,
            };
        }

        Self::day(week_constraint(area))
    }

    pub const fn should_render_month_grid(self) -> bool {
        matches!(self.mode, ResponsiveMode::MonthGrid)
    }

    pub const fn should_render_week_view(self) -> bool {
        matches!(self.mode, ResponsiveMode::WeekView)
    }

    const fn day(constraint: LayoutConstraint) -> Self {
        Self {
            mode: ResponsiveMode::DayFallback,
            constraint,
        }
    }
}

const fn month_constraint(area: Rect) -> LayoutConstraint {
    if is_portrait(area) {
        return LayoutConstraint::Portrait;
    }

    match (
        area.width < MIN_MONTH_GRID_WIDTH,
        area.height < MIN_MONTH_GRID_HEIGHT,
    ) {
        (true, true) => LayoutConstraint::MonthTooNarrowAndShort,
        (true, false) => LayoutConstraint::MonthTooNarrow,
        (false, true) => LayoutConstraint::MonthTooShort,
        (false, false) => LayoutConstraint::FitsMonthGrid,
    }
}

const fn week_constraint(area: Rect) -> LayoutConstraint {
    match (
        area.width < MIN_WEEK_VIEW_WIDTH,
        area.height < MIN_WEEK_VIEW_HEIGHT,
    ) {
        (true, true) => LayoutConstraint::WeekTooNarrowAndShort,
        (true, false) => LayoutConstraint::WeekTooNarrow,
        (false, true) => LayoutConstraint::WeekTooShort,
        (false, false) => month_constraint(area),
    }
}

const fn is_portrait(area: Rect) -> bool {
    area.width < area.height.saturating_mul(PORTRAIT_WIDTH_TO_HEIGHT_RATIO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_landscape_size_uses_month_grid() {
        let layout = ResponsiveLayout::for_area(Rect::new(0, 0, 84, 26));

        assert_eq!(layout.mode, ResponsiveMode::MonthGrid);
        assert_eq!(layout.constraint, LayoutConstraint::FitsMonthGrid);
    }

    #[test]
    fn threshold_size_can_use_month_grid() {
        let layout = ResponsiveLayout::for_area(Rect::new(
            0,
            0,
            MIN_MONTH_GRID_WIDTH,
            MIN_MONTH_GRID_HEIGHT,
        ));

        assert!(layout.should_render_month_grid());
    }

    #[test]
    fn portrait_size_that_still_fits_month_keeps_month_grid() {
        let layout = ResponsiveLayout::for_area(Rect::new(0, 0, 60, 40));

        assert_eq!(layout.mode, ResponsiveMode::MonthGrid);
        assert_eq!(layout.constraint, LayoutConstraint::FitsMonthGrid);
    }

    #[test]
    fn moderate_pressure_uses_week_view() {
        assert_eq!(
            ResponsiveLayout::for_area(Rect::new(0, 0, 84, MIN_MONTH_GRID_HEIGHT - 1)),
            ResponsiveLayout {
                mode: ResponsiveMode::WeekView,
                constraint: LayoutConstraint::MonthTooShort
            }
        );
        assert_eq!(
            ResponsiveLayout::for_area(Rect::new(0, 0, MIN_MONTH_GRID_WIDTH - 1, 26)),
            ResponsiveLayout {
                mode: ResponsiveMode::WeekView,
                constraint: LayoutConstraint::Portrait
            }
        );
    }

    #[test]
    fn week_view_has_its_own_lower_thresholds() {
        let layout =
            ResponsiveLayout::for_area(Rect::new(0, 0, MIN_WEEK_VIEW_WIDTH, MIN_WEEK_VIEW_HEIGHT));

        assert!(layout.should_render_week_view());
    }

    #[test]
    fn tiny_sizes_use_day_fallback() {
        assert_eq!(
            ResponsiveLayout::for_area(Rect::new(0, 0, MIN_WEEK_VIEW_WIDTH - 1, 26)),
            ResponsiveLayout {
                mode: ResponsiveMode::DayFallback,
                constraint: LayoutConstraint::WeekTooNarrow
            }
        );
        assert_eq!(
            ResponsiveLayout::for_area(Rect::new(0, 0, 84, MIN_WEEK_VIEW_HEIGHT - 1)),
            ResponsiveLayout {
                mode: ResponsiveMode::DayFallback,
                constraint: LayoutConstraint::WeekTooShort
            }
        );
    }
}
