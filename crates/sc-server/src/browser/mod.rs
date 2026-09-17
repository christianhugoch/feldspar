//! The headless browser behind the coding agent's `view_app` (TODO §7b).
//!
//! - [`detect`] finds the binary at boot.
//! - [`snapshot`] renders a page's accessibility tree for the model.

mod detect;
mod driver;
pub mod snapshot;

pub use driver::{ChromiumDriver, DriverConfig};

pub use detect::{BROWSER_NAMES, detect_browser};
