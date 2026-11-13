//! Which surface is waiting on which request.
//!
//! The daemon multiplexes surfaces: it holds one adapter connection and N
//! surface connections, so a reply that arrives from the adapter has to be
//! handed to the surface that asked for it rather than to everybody. The
//! protocol's only addressing for that is the `requestId` in the body — the
//! adapter mints a fresh header `frameId` for every frame it writes, so a
//! request's own id never comes back.
//!
//! This lives here rather than in the daemon binary because it is the one part
//! of multiplexing that is pure logic, and pure logic is what can be tested
//! without spawning a daemon and an adapter. The failure it guards against is
//! silent: a reply delivered to the wrong surface leaves the surface that asked
//! waiting forever, because `await_reply` has no timeout of its own.

use std::collections::HashMap;

/// The routing table: request id to the surface waiting on it.
///
/// A surface is identified by the daemon's own monotonic serial, because an
/// `mpsc::Sender` has no identity to compare.
#[derive(Debug, Default)]
pub struct Routes {
    waiting: HashMap<String, u64>,
}

impl Routes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember that `surface` is waiting on `request_id`.
    ///
    /// Returns the surface that was waiting there before, if any. That is a
    /// protocol violation rather than a state to absorb: two surfaces minted
    /// the same id, so one of them is about to wait for a reply that will be
    /// handed to the other. The caller logs it, because the alternative is a
    /// surface that hangs with nothing in any log to say why.
    pub fn expect(&mut self, request_id: &str, surface: u64) -> Option<u64> {
        self.waiting.insert(request_id.to_string(), surface)
    }

    /// Take the surface waiting on `request_id`.
    ///
    /// `None` means nobody is waiting on it, which is the normal case for an
    /// event: the event envelope carries no `requestId`, so an event must be
    /// broadcast rather than routed.
    pub fn settle(&mut self, request_id: &str) -> Option<u64> {
        self.waiting.remove(request_id)
    }

    /// Forget everything a departing surface was waiting for.
    ///
    /// A surface that left cannot have its in-flight requests answered, and
    /// keeping them would grow the table once per surface that ever attached,
    /// for as long as the daemon lives.
    pub fn forget(&mut self, surface: u64) {
        self.waiting.retain(|_, held| *held != surface);
    }

    /// How many requests are still unanswered. For tests and for a diagnostic
    /// that would otherwise have to reach into the map.
    pub fn len(&self) -> usize {
        self.waiting.len()
    }

    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_routed_to_the_surface_that_asked() {
        let mut routes = Routes::new();
        assert_eq!(routes.expect("shell-1-1", 7), None, "nobody was waiting");
        assert_eq!(routes.settle("shell-1-1"), Some(7));
        // Settled, so a second frame with the same id is not an answer to
        // anybody: the adapter would have to send the same receipt twice.
        assert_eq!(routes.settle("shell-1-1"), None);
    }

    #[test]
    fn an_unknown_request_is_not_an_answer_to_anybody() {
        // The event case. The event envelope has no `requestId`, so nothing is
        // ever registered for one, and routing one would hand a broadcast to a
        // single surface.
        let mut routes = Routes::new();
        routes.expect("shell-1-1", 7);
        assert_eq!(routes.settle("studio.event"), None);
    }

    #[test]
    fn two_surfaces_are_routed_separately() {
        let mut routes = Routes::new();
        routes.expect("shell-1-1", 7);
        routes.expect("shell-2-1", 9);
        // Out of order, which is what concurrent surfaces produce: the adapter
        // answers whichever request finishes first.
        assert_eq!(routes.settle("shell-2-1"), Some(9));
        assert_eq!(routes.settle("shell-1-1"), Some(7));
        assert!(routes.is_empty());
    }

    #[test]
    fn a_repeated_id_is_reported_rather_than_absorbed() {
        // The bug this pins: request ids were a per-session counter, so two
        // surfaces minted the same one. Absorbing it silently is what made the
        // failure undiagnosable — the surface that lost its slot waited for a
        // reply that went to the other one, with nothing in any log.
        let mut routes = Routes::new();
        assert_eq!(routes.expect("shell-1-1", 7), None);
        assert_eq!(
            routes.expect("shell-1-1", 9),
            Some(7),
            "the displaced surface is reported, so the caller can say so"
        );
        // And the newcomer is the one that gets the reply, which is exactly why
        // the displacement has to be loud.
        assert_eq!(routes.settle("shell-1-1"), Some(9));
    }

    #[test]
    fn a_departed_surface_stops_being_routed_to() {
        let mut routes = Routes::new();
        routes.expect("shell-1-1", 7);
        routes.expect("shell-1-2", 7);
        routes.expect("shell-2-1", 9);
        routes.forget(7);
        assert_eq!(routes.settle("shell-1-1"), None);
        assert_eq!(routes.settle("shell-1-2"), None);
        assert_eq!(
            routes.settle("shell-2-1"),
            Some(9),
            "the other surface is untouched"
        );
    }

    #[test]
    fn the_table_does_not_grow_when_surfaces_leave() {
        // One entry per unanswered request, and nothing left behind by a
        // surface that is gone. A leak here is one entry per surface that ever
        // attached, for the daemon's lifetime.
        let mut routes = Routes::new();
        for surface in 1..=50u64 {
            routes.expect(&format!("shell-{surface}-1"), surface);
        }
        assert_eq!(routes.len(), 50, "one entry per unanswered request");
        for surface in 1..=50u64 {
            routes.forget(surface);
        }
        assert!(
            routes.is_empty(),
            "every departing surface took its requests with it"
        );
    }
}
