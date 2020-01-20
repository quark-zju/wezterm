// The range_plus_one lint can't see when the LHS is not compatible with
// and inclusive range
#![cfg_attr(feature = "cargo-clippy", allow(clippy::range_plus_one))]
use crate::mux::renderable::Renderable;
use std::ops::Range;
use term::StableRowIndex;
use termwiz::surface::line::DoubleClickRange;

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct Selection {
    /// Remembers the starting coordinate of the selection prior to
    /// dragging.
    pub start: Option<SelectionCoordinate>,
    /// Holds the not-normalized selection range.
    pub range: Option<SelectionRange>,
}

impl Selection {
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.range = None;
        self.start = None;
    }

    pub fn begin(&mut self, start: SelectionCoordinate) {
        self.range = None;
        self.start = Some(start);
    }

    pub fn is_empty(&self) -> bool {
        self.range.is_none()
    }
}

/// The x,y coordinates of either the start or end of a selection region
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct SelectionCoordinate {
    pub x: usize,
    pub y: StableRowIndex,
}

/// Represents the selected text range.
/// The end coordinates are inclusive.
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
pub struct SelectionRange {
    pub start: SelectionCoordinate,
    pub end: SelectionCoordinate,
}

// TODO: expose is_double_click_word in config file
fn is_double_click_word(s: &str) -> bool {
    match s.len() {
        1 => match s.chars().nth(0).unwrap() {
            ' ' | '\t' | '\n' | '{' | '[' | '}' | ']' | '(' | ')' | '"' | '\'' => false,
            _ => true,
        },
        0 => false,
        _ => true,
    }
}

impl SelectionRange {
    /// Create a new range that starts at the specified location
    pub fn start(start: SelectionCoordinate) -> Self {
        let end = start;
        Self { start, end }
    }

    /// Computes the selection range for the line around the specified coords
    pub fn line_around(start: SelectionCoordinate) -> Self {
        Self {
            start: SelectionCoordinate { x: 0, y: start.y },
            end: SelectionCoordinate {
                x: usize::max_value(),
                y: start.y,
            },
        }
    }

    /// Computes the selection range for the word around the specified coords
    pub fn word_around(start: SelectionCoordinate, renderer: &mut dyn Renderable) -> Self {
        let (first, lines) = renderer.get_lines(start.y..start.y + 1);

        // TODO: if selection_range.start.x == 0, search backwards for wrapping
        // lines too.

        match lines[0].compute_double_click_range(start.x, is_double_click_word) {
            DoubleClickRange::Range(click_range) => Self {
                start: SelectionCoordinate {
                    x: click_range.start,
                    y: first,
                },
                end: SelectionCoordinate {
                    x: click_range.end - 1,
                    y: first,
                },
            },
            DoubleClickRange::RangeWithWrap(range_start) => {
                let start_coord = SelectionCoordinate {
                    x: range_start.start,
                    y: first,
                };

                let mut end_coord = SelectionCoordinate {
                    x: range_start.end - 1,
                    y: first,
                };

                for y_cont in start.y + 1.. {
                    let (first, lines) = renderer.get_lines(y_cont..y_cont + 1);
                    if first != y_cont {
                        break;
                    }
                    match lines[0].compute_double_click_range(0, is_double_click_word) {
                        DoubleClickRange::Range(range_end) => {
                            if range_end.end > range_end.start {
                                end_coord = SelectionCoordinate {
                                    x: range_end.end - 1,
                                    y: y_cont,
                                };
                            }
                            break;
                        }
                        DoubleClickRange::RangeWithWrap(range_end) => {
                            end_coord = SelectionCoordinate {
                                x: range_end.end - 1,
                                y: y_cont,
                            };
                        }
                    }
                }

                Self {
                    start: start_coord,
                    end: end_coord,
                }
            }
        }
    }

    /// Extends the current selection by unioning it with another selection range
    pub fn extend_with(&self, other: Self) -> Self {
        let norm = self.normalize();
        let other = other.normalize();
        let (start, end) = if (norm.start.y < other.start.y)
            || (norm.start.y == other.start.y && norm.start.x <= other.start.x)
        {
            (norm, other)
        } else {
            (other, norm)
        };
        Self {
            start: start.start,
            end: end.end,
        }
    }

    /// Returns an extended selection that it ends at the specified location
    pub fn extend(&self, end: SelectionCoordinate) -> Self {
        Self {
            start: self.start,
            end,
        }
    }

    /// Return a normalized selection such that the starting y coord
    /// is <= the ending y coord.
    pub fn normalize(&self) -> Self {
        if self.start.y <= self.end.y {
            *self
        } else {
            Self {
                start: self.end,
                end: self.start,
            }
        }
    }

    /// Yields a range representing the row indices.
    /// Make sure that you invoke this on a normalized range!
    pub fn rows(&self) -> Range<StableRowIndex> {
        let norm = self.normalize();
        norm.start.y..norm.end.y + 1
    }

    /// Yields a range representing the selected columns for the specified row.
    /// Not that the range may include usize::max_value() for some rows; this
    /// indicates that the selection extends to the end of that row.
    /// Since this struct has no knowledge of line length, it cannot be
    /// more precise than that.
    /// Must be called on a normalized range!
    pub fn cols_for_row(&self, row: StableRowIndex) -> Range<usize> {
        let norm = self.normalize();
        if row < norm.start.y || row > norm.end.y {
            0..0
        } else if norm.start.y == norm.end.y {
            // A single line selection
            if norm.start.x <= norm.end.x {
                norm.start.x..norm.end.x.saturating_add(1)
            } else {
                norm.end.x..norm.start.x.saturating_add(1)
            }
        } else if row == norm.end.y {
            // last line of multi-line
            0..norm.end.x.saturating_add(1)
        } else if row == norm.start.y {
            // first line of multi-line
            norm.start.x..usize::max_value()
        } else {
            // some "middle" line of multi-line
            0..usize::max_value()
        }
    }
}
