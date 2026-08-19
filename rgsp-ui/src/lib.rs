//! On-device UI for rgsp-cast, drawn with NextUI's own C toolkit.
//!
//! [`sys`] is the raw bindgen layer; [`ui::Ui`] is the safe wrapper screens
//! are built on.

pub mod sys;
pub mod ui;
