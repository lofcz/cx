//! Account usage belongs to the persistent bottom footer, never the composer gap.

use super::*;
use crate::terminal_hyperlinks::HyperlinkLine;

impl App {
    pub(super) fn composer_hint(&self, _width: u16) -> Option<HyperlinkLine> {
        None
    }
}

#[cfg(test)]
#[path = "composer_hints_tests.rs"]
mod tests;
