//! Home screen: one vertical list, the service row first, then one row per
//! pending connection.
//!
//! [`Home::update`] is pure — no [`Ui`], no FFI — so it unit-tests
//! off-device; only [`Home::draw`] touches `Ui`.

use crate::rpc::CastState;
use crate::ui::{Buttons, Ui};

/// What the caller should do in response to this frame's input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeAction {
    /// Nothing to act on this frame.
    None,
    /// `A` on the service row: start it if stopped, stop it if running.
    Toggle,
    /// `A` on a pending row: open PIN entry for this client id.
    Pair(String),
    /// `B`: leave the app.
    Exit,
}

/// The home screen's cursor position. Just an index into a list that is
/// reconstructed from `state` on every call — there is nothing else to
/// track, and keeping no cached copy of the list is what makes clamping
/// against the live `state` (see [`Home::update`]) correct by construction.
pub struct Home {
    selected: usize,
}

impl Home {
    pub fn new() -> Home {
        Home { selected: 0 }
    }

    /// Advance the cursor and/or act on the current selection.
    ///
    /// The cursor is clamped against `state` before anything else runs, on
    /// every call — not just when the list shrinks visibly. The daemon can
    /// remove a pending client (pairing completes or is abandoned) between
    /// two frames while the cursor still sits on that row, so clamping
    /// against a remembered row count would let `A` act on a row that no
    /// longer exists. Clamping against the row count `state` reports *right
    /// now* is what makes that impossible.
    pub fn update(&mut self, buttons: &Buttons, state: &CastState) -> HomeAction {
        let row_count = 1 + state.pending.len();
        if self.selected >= row_count {
            self.selected = row_count - 1;
        }

        if buttons.b {
            return HomeAction::Exit;
        }

        if buttons.down && self.selected + 1 < row_count {
            self.selected += 1;
        } else if buttons.up && self.selected > 0 {
            self.selected -= 1;
        }

        if buttons.a {
            return match self.selected {
                0 => HomeAction::Toggle,
                i => HomeAction::Pair(state.pending[i - 1].id.clone()),
            };
        }

        HomeAction::None
    }

    /// What the `A` button's hint label should read for the current
    /// selection: `Pair` on a pending row (that's what `A` does there),
    /// `Start`/`Stop` on the service row, mirroring `casting`. Pure — no
    /// `Ui` — so this stays unit-testable alongside `update`, the same
    /// FFI-free split. The hint bar is the only affordance this device has
    /// for what a button does, so it must always name the action `A` is
    /// actually about to take, not just what it would do on a different row.
    fn a_hint(&self, state: &CastState) -> &'static str {
        if self.selected == 0 {
            if state.casting { "Stop" } else { "Start" }
        } else {
            "Pair"
        }
    }

    pub fn draw(&self, ui: &mut Ui, state: &CastState) {
        ui.header("Cast");

        let status = if state.casting { "Running" } else { "Stopped" };
        ui.row("Service", Some(status), 0, self.selected == 0);

        for (i, entry) in state.pending.iter().enumerate() {
            let label = crate::screens::client_label(entry.name.as_deref(), &entry.id);
            ui.row(&label, Some(">"), (i + 1) as i32, self.selected == i + 1);
        }

        ui.hints(&[("A", self.a_hint(state)), ("B", "Exit")]);
    }
}

impl Default for Home {
    fn default() -> Home {
        Home::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::PendingEntry;

    fn down() -> Buttons {
        Buttons { down: true, ..Default::default() }
    }

    fn a() -> Buttons {
        Buttons { a: true, ..Default::default() }
    }

    fn b() -> Buttons {
        Buttons { b: true, ..Default::default() }
    }

    #[test]
    fn selection_moves_across_service_row_and_pending_rows() {
        let state = CastState {
            casting: true,
            client: None,
            pending: vec![
                PendingEntry { id: "AA".into(), name: Some("eric-mbp".into()) },
                PendingEntry { id: "BB".into(), name: None },
            ],
        };
        let mut home = Home::new();
        assert_eq!(home.update(&down(), &state), HomeAction::None);
        assert_eq!(home.update(&a(), &state), HomeAction::Pair("AA".into()));
    }

    #[test]
    fn a_on_the_service_row_toggles() {
        let state = CastState { casting: false, client: None, pending: vec![] };
        let mut home = Home::new();
        assert_eq!(home.update(&a(), &state), HomeAction::Toggle);
    }

    #[test]
    fn selection_clamps_when_pending_clients_disappear() {
        let two = CastState {
            casting: true,
            client: None,
            pending: vec![
                PendingEntry { id: "AA".into(), name: None },
                PendingEntry { id: "BB".into(), name: None },
            ],
        };
        let none = CastState { casting: true, client: None, pending: vec![] };
        let mut home = Home::new();
        home.update(&down(), &two);
        home.update(&down(), &two);
        // the list shrank under the cursor; A must not panic or pair a ghost
        assert_eq!(home.update(&a(), &none), HomeAction::Toggle);
    }

    #[test]
    fn selection_does_not_move_past_the_service_row_when_pending_is_empty() {
        let state = CastState { casting: false, client: None, pending: vec![] };
        let mut home = Home::new();
        home.update(&down(), &state);
        assert_eq!(home.update(&a(), &state), HomeAction::Toggle);
    }

    #[test]
    fn a_hint_tracks_the_selection() {
        let state = CastState {
            casting: true,
            client: None,
            pending: vec![PendingEntry { id: "AA".into(), name: None }],
        };
        let mut home = Home::new();
        // service row selected: hint mirrors `casting`, not "Pair"
        assert_eq!(home.a_hint(&state), "Stop");

        home.update(&down(), &state);
        // pending row selected: A pairs here, not toggles the service
        assert_eq!(home.a_hint(&state), "Pair");

        let stopped = CastState { casting: false, ..state };
        let home = Home::new();
        assert_eq!(home.a_hint(&stopped), "Start");
    }

    #[test]
    fn b_exits() {
        let state = CastState { casting: false, client: None, pending: vec![] };
        let mut home = Home::new();
        assert_eq!(home.update(&b(), &state), HomeAction::Exit);
    }
}
