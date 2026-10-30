//! Progress display on stderr, hidden automatically when it isn't a terminal.

use indicatif::{ProgressBar, ProgressStyle};
use std::fmt::Display;
use std::time::Duration;

/// A spinner for work whose total isn't known yet.
pub fn spinner(template: &str) -> ProgressBar {
    ticking(ProgressBar::new_spinner(), ProgressStyle::with_template(template).unwrap())
}

/// A bar counting up to `len`.
pub fn bar(len: usize, template: &str) -> ProgressBar {
    let style = ProgressStyle::with_template(template).unwrap().progress_chars("=> ");
    ticking(ProgressBar::new(len as u64), style)
}

/// Prints a warning without garbling `bar`. Unlike `bar.println`, this still
/// prints when the bar is hidden because stderr isn't a terminal.
pub fn warn(bar: &ProgressBar, msg: impl Display) {
    bar.suspend(|| eprintln!("warning: {msg}"));
}

fn ticking(bar: ProgressBar, style: ProgressStyle) -> ProgressBar {
    let bar = bar.with_style(style);
    bar.enable_steady_tick(Duration::from_millis(100));
    bar
}
