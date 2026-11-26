//! The before-you-print list.
//!
//! Guarantees:
//! - every line is a sentence of the catalogue; nothing else can be put on the list;
//! - the list shows at most [`Checklist::limit`] lines, unless the lines that are never
//!   dropped are more. When lines must go, they go from the highest class number, the last
//!   line of that class first;
//! - a line of the classes 1 to 3 (plate, feed and loading, which file to open) is never
//!   dropped, and neither is a line of [`ALWAYS_SHOWN`]: the line that protects a mating
//!   face from its brim, how a flexible part comes off the plate, the question whether every
//!   piece stayed down, which the print history rests on, the warning about a crowded
//!   flexible plate, and the calibration level 3 asks for.

use crate::explain::{Catalogue, Places, PlanError};
use crate::model::{Checklist, ChecklistItem, Condition};

/// The highest class whose lines are never dropped.
pub const LAST_KEPT_CLASS: u8 = 3;
/// Lines of the classes 4 to 6 that are never dropped either, by id. A plan carries at most
/// one of the `after.` lines.
pub const ALWAYS_SHOWN: &[&str] = &[
    "run_calibration",
    "few_pieces",
    "after.brim_fit",
    "after.flexible",
    "after.flexible_brim",
    "outcome",
];
/// The length of the list for a rigid material.
pub const LIMIT: usize = 8;
/// The length of the list for a flexible material, which needs the hand-loading lines.
pub const LIMIT_FLEXIBLE: usize = 10;
/// Level 3 adds three lines that only it has (check the plate, glue, run the calibration);
/// the list is that much longer there, so that they do not push out the others.
pub const LEVEL_3_EXTRA: usize = 3;

impl Checklist {
    /// The lines to show. `print_minutes` is the print time the slicer reported, when it is
    /// known: a line that applies only to long prints is kept while the time is unknown.
    /// `project_delivered` says whether the editable project is handed over as well.
    pub fn lines(
        &self,
        print_minutes: Option<u32>,
        project_delivered: bool,
    ) -> Vec<&ChecklistItem> {
        let mut lines: Vec<&ChecklistItem> = self
            .items
            .iter()
            .filter(|item| match item.condition {
                None => true,
                Some(Condition::PrintLongerThanMinutes(minutes)) => {
                    print_minutes.is_none_or(|known| known > minutes)
                }
                Some(Condition::ProjectDelivered) => project_delivered,
            })
            .collect();
        // The items are sorted by class and then as in the catalogue, so the line to drop
        // is the last one that may go.
        while lines.len() > self.limit {
            let Some(at) = lines.iter().rposition(|line| !Self::never_dropped(line)) else {
                break;
            };
            lines.remove(at);
        }
        lines
    }

    /// Whether a line is shown however long the list gets.
    pub fn never_dropped(item: &ChecklistItem) -> bool {
        item.class <= LAST_KEPT_CLASS || ALWAYS_SHOWN.contains(&item.id)
    }

    /// An empty list, for a plan that is blocked.
    pub(crate) fn empty() -> Self {
        Self {
            items: Vec::new(),
            limit: LIMIT,
        }
    }
}

/// Collects the lines of a plan.
pub(crate) struct ListBuilder<'a> {
    catalogue: &'a Catalogue,
    items: Vec<(u8, usize, ChecklistItem)>,
}

impl<'a> ListBuilder<'a> {
    pub(crate) fn new(catalogue: &'a Catalogue) -> Self {
        Self {
            catalogue,
            items: Vec::new(),
        }
    }

    /// Adds a line by its catalogue id. A line that is already there is not added twice.
    pub(crate) fn add(
        &mut self,
        id: &str,
        filled: Places<'_>,
        condition: Option<Condition>,
    ) -> Result<(), PlanError> {
        let (position, item) = self.catalogue.line(id, filled, condition)?;
        if !self.items.iter().any(|(_, _, known)| known.id == item.id) {
            self.items.push((item.class, position, item));
        }
        Ok(())
    }

    /// The list, by class and then in the order of the catalogue.
    pub(crate) fn finish(mut self, limit: usize) -> Checklist {
        self.items
            .sort_by_key(|(class, position, _)| (*class, *position));
        Checklist {
            items: self.items.into_iter().map(|(_, _, item)| item).collect(),
            limit,
        }
    }
}
