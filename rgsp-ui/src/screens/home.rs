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

/// How many rows — the service row plus pending rows — actually get drawn.
/// Capped at [`crate::ui::MAIN_ROW_COUNT`]: NextUI reserves that many
/// `PILL_SIZE` rows total once the top status band and bottom hint band are
/// accounted for, and drawing past it would run rows under the hint group
/// and off the bottom of the panel. No scrolling: realistically there is
/// one pending client, occasionally two, so simply not drawing past the cap
/// is enough — a client beyond it just has no row until an earlier one
/// clears (paired or abandoned), the same as it would if it hadn't
/// connected yet.
///
/// A free function, not a method, so both [`Home::update`] (to clamp the
/// cursor) and [`Home::draw`] (to cap the loop) call the exact same
/// computation — they can never disagree about how many rows exist.
fn visible_row_count(state: &CastState) -> usize {
    (1 + state.pending.len()).min(crate::ui::MAIN_ROW_COUNT as usize)
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
    /// now* is what makes that impossible. That row count is also capped at
    /// [`visible_row_count`] — the cursor must never land on a row `draw`
    /// does not actually draw, on-screen or off.
    ///
    /// `A` on the service row always returns `Toggle` — which direction
    /// that means (start vs. stop) is decided by the caller from socket
    /// connectivity, not here; see [`Home::a_hint`] and [`Home::status_text`]
    /// for where connectivity actually changes what's shown.
    pub fn update(&mut self, buttons: &Buttons, state: &CastState) -> HomeAction {
        let row_count = visible_row_count(state);
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
    /// `Start`/`Stop` on the service row, mirroring `connected` -- the
    /// daemon's liveness, not `casting`. Pure — no `Ui` — so this stays
    /// unit-testable alongside `update`, the same FFI-free split. The hint
    /// bar is the only affordance this device has for what a button does,
    /// so it must always name the action `A` is actually about to take, not
    /// just what it would do on a different row.
    fn a_hint(&self, connected: bool) -> &'static str {
        if self.selected == 0 {
            if connected { "Stop" } else { "Start" }
        } else {
            "Pair"
        }
    }

    /// The service row's text: three honest states, not two. The socket
    /// being down means the daemon isn't running at all (`Stopped`); the
    /// socket being up but no stream in progress means it's running idle
    /// (`Running`); and `casting` on top of that means a client is actively
    /// streaming (`Casting`, with the client's name if `state.client` has
    /// one). Collapsing this to just `casting` (the old behavior) read
    /// "Stopped" for a daemon that was up and simply idle, which is the
    /// normal state right after starting it. Pure, and split out of `draw`
    /// so it stays unit-testable without a `Ui`, same as `a_hint`.
    fn status_text(state: &CastState, connected: bool) -> String {
        if !connected {
            "Stopped".to_string()
        } else if state.casting {
            match state.client.as_deref() {
                Some(name) => format!("Casting ({name})"),
                None => "Casting".to_string(),
            }
        } else {
            "Running".to_string()
        }
    }

    pub fn draw(&self, ui: &mut Ui, state: &CastState, connected: bool) {
        ui.hardware_group();
        ui.header("Cast");

        let status = Self::status_text(state, connected);
        ui.row("Service", Some(&status), 0, self.selected == 0);

        // The service row above already claimed one of visible_row_count's
        // slots, so at most `visible_row_count(state) - 1` pending rows fit.
        let max_pending_rows = visible_row_count(state) - 1;
        for (i, entry) in state.pending.iter().enumerate().take(max_pending_rows) {
            let label = crate::screens::client_label(entry.name.as_deref(), entry.address.as_deref(), &entry.id);
            ui.row(&label, None, (i + 1) as i32, self.selected == i + 1);
        }

        ui.hints(&[("A", self.a_hint(connected)), ("B", "Exit")]);
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
                PendingEntry { id: "AA".into(), name: Some("eric-mbp".into()), address: None },
                PendingEntry { id: "BB".into(), name: None, address: None },
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
                PendingEntry { id: "AA".into(), name: None, address: None },
                PendingEntry { id: "BB".into(), name: None, address: None },
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
    fn a_hint_tracks_the_selection_and_connectivity() {
        let state = CastState {
            casting: true,
            client: None,
            pending: vec![PendingEntry { id: "AA".into(), name: None, address: None }],
        };
        let mut home = Home::new();
        // service row selected, connected: A stops the daemon
        assert_eq!(home.a_hint(true), "Stop");
        // service row selected, disconnected: A starts it, regardless of `casting`
        assert_eq!(home.a_hint(false), "Start");

        home.update(&down(), &state);
        // pending row selected: A pairs here, not toggles the service
        assert_eq!(home.a_hint(true), "Pair");
    }

    #[test]
    fn b_exits() {
        let state = CastState { casting: false, client: None, pending: vec![] };
        let mut home = Home::new();
        assert_eq!(home.update(&b(), &state), HomeAction::Exit);
    }

    #[test]
    fn connected_and_not_casting_reads_running_and_offers_stop() {
        let state = CastState { casting: false, client: None, pending: vec![] };
        let mut home = Home::new();
        assert_eq!(Home::status_text(&state, true), "Running");
        assert_eq!(home.update(&a(), &state), HomeAction::Toggle);
        assert_eq!(home.a_hint(true), "Stop");
    }

    #[test]
    fn connected_and_casting_reads_casting_with_the_client_name() {
        // `casting` on top of a live socket is a third state ("Casting"),
        // not the same row text as an idle-but-connected daemon.
        let anonymous = CastState { casting: true, client: None, pending: vec![] };
        let named = CastState { casting: true, client: Some("eric-mbp".into()), pending: vec![] };
        assert_eq!(Home::status_text(&anonymous, true), "Casting");
        assert_eq!(Home::status_text(&named, true), "Casting (eric-mbp)");
    }

    #[test]
    fn disconnected_reads_stopped_and_offers_start_regardless_of_stale_casting_state() {
        // The socket being down authoritatively means "stopped", even if
        // the last state we polled before losing the connection said
        // `casting: true` -- a stale flag from before disconnect must not
        // override the liveness signal.
        let stale = CastState { casting: true, client: Some("eric-mbp".into()), pending: vec![] };
        let mut home = Home::new();
        assert_eq!(Home::status_text(&stale, false), "Stopped");
        assert_eq!(home.update(&a(), &stale), HomeAction::Toggle);
        assert_eq!(home.a_hint(false), "Start");
    }

    #[test]
    fn visible_row_count_caps_at_main_row_count() {
        let cap = crate::ui::MAIN_ROW_COUNT as usize;
        let plenty: Vec<PendingEntry> =
            (0..cap).map(|i| PendingEntry { id: format!("id{i}"), name: None, address: None }).collect();
        let over_cap = CastState { casting: false, client: None, pending: plenty };
        assert_eq!(visible_row_count(&over_cap), cap, "service row + cap - 1 pending rows, not more");

        let one = CastState {
            casting: false,
            client: None,
            pending: vec![PendingEntry { id: "AA".into(), name: None, address: None }],
        };
        assert_eq!(visible_row_count(&one), 2, "well under the cap: service row + the one pending row");
    }

    #[test]
    fn selection_and_pairing_never_cross_the_visible_row_cap() {
        // More pending clients than MAIN_ROW_COUNT can show. Without a cap,
        // repeated `down` would walk the cursor onto a row draw() never
        // actually draws (past the hint bar, off the bottom of the panel),
        // and `A` there would pair a client whose row was never visible.
        let cap = crate::ui::MAIN_ROW_COUNT as usize - 1; // pending rows that fit
        let pending: Vec<PendingEntry> =
            (0..cap + 3).map(|i| PendingEntry { id: format!("id{i}"), name: None, address: None }).collect();
        let state = CastState { casting: false, client: None, pending };
        let mut home = Home::new();

        for _ in 0..cap + 5 {
            home.update(&down(), &state);
        }

        // The cursor stops at the last row that is actually drawn: the
        // service row plus `cap` pending rows, so the last pending row is
        // index `cap - 1`, not one of the clients past the cap.
        assert_eq!(home.update(&a(), &state), HomeAction::Pair(format!("id{}", cap - 1)));
    }
}
