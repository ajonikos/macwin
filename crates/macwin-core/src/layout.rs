//! Edge mapping between my screens and the peer's screens.
//!
//! Let P be my `peer_side` and F = P.opposite() the side of the peer's
//! screens that faces me. Positions are mapped proportionally along the
//! bounding boxes; crossings are detected on the free edge of the screen
//! under the cursor so laptop + external-monitor arrangements work.

use crate::geometry::ScreenSet;
use crate::types::{Point, PointF, Side};

/// How far (native units) a cursor returned to my screens lands inside the
/// edge, so the next local event does not immediately re-enter the peer.
pub const EDGE_REENTRY_INSET: i32 = 2;

/// Result of a successful edge crossing from my screens onto the peer's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossing {
    /// Virtual cursor start, peer native coordinates.
    pub entry: PointF,
    /// Where my cursor left (parked), my native coordinates.
    pub exit: Point,
    /// `native_per_logical` of my screen containing `exit`.
    pub npl_src: f64,
}

/// One virtual-cursor step on the peer's screens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// The virtual cursor moved; the point is on a peer screen.
    Moved(PointF),
    /// The cursor crossed back onto my screens; warp my cursor here.
    Exited { warp_to: Point },
}

/// Static mapping for one session.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    local: ScreenSet,
    peer: ScreenSet,
    peer_side: Side,
}

impl Layout {
    pub fn new(local: ScreenSet, peer: ScreenSet, peer_side: Side) -> Layout {
        Layout {
            local,
            peer,
            peer_side,
        }
    }

    pub fn local(&self) -> &ScreenSet {
        &self.local
    }

    pub fn peer(&self) -> &ScreenSet {
        &self.peer
    }

    pub fn peer_side(&self) -> Side {
        self.peer_side
    }

    /// The side of the peer's screens that faces me.
    pub fn facing(&self) -> Side {
        self.peer_side.opposite()
    }

    /// Maps a local y onto the peer's bounding box.
    pub fn map_y_to_peer(&self, y: f64) -> f64 {
        self.peer.y_at(self.local.t(y))
    }

    /// Maps a peer y onto my bounding box.
    pub fn map_y_to_local(&self, y: f64) -> f64 {
        self.local.y_at(self.peer.t(y))
    }

    /// If the local cursor at `pos` sits on my free P edge, the crossing onto
    /// the peer. Other preconditions (session, capabilities, buttons) are the
    /// Engine's business.
    pub fn try_enter(&self, pos: PointF) -> Option<Crossing> {
        let p = self.local.clamp_f(pos);
        if !self.local.at_free_edge(p, self.peer_side) {
            return None;
        }
        let y_t = self.map_y_to_peer(p.y);
        let f = self.facing();
        let (i, y) = self.peer.face(y_t, f);
        let r = self.peer.screens()[i].rect;
        let x = match f {
            Side::Left => r.left(),
            Side::Right => r.right() - 1,
        };
        let exit = p.floor();
        Some(Crossing {
            entry: PointF::new(f64::from(x), y),
            exit,
            npl_src: self.local.screen_for(exit.to_f()).native_per_logical,
        })
    }

    /// Moves the virtual cursor `v` (on a peer screen) by the local delta
    /// `delta`, normalising speed by the two screens' scales. Leaving is
    /// suppressed while `buttons_held` (no leaving mid-drag).
    pub fn step(&self, v: PointF, delta: PointF, npl_src: f64, buttons_held: bool) -> Step {
        let v = self.peer.clamp_f(v);
        let s = *self.peer.screen_for(v);
        let npl_tgt = s.native_per_logical;
        // d_t = d_s / npl_src * npl_tgt; exact identity for equal scales and
        // multiply-then-divide otherwise (3 px at 1.5 -> exactly 2 points).
        let d = if !delta.is_finite() {
            PointF::default()
        } else if npl_tgt == npl_src {
            delta
        } else {
            PointF::new(delta.x * npl_tgt / npl_src, delta.y * npl_tgt / npl_src)
        };
        let next = PointF::new(v.x + d.x, v.y + d.y);
        if self.peer.contains_f(next) {
            return Step::Moved(next);
        }
        let r = s.rect;
        let crosses = match self.facing() {
            Side::Left => next.x < f64::from(r.left()),
            Side::Right => next.x >= f64::from(r.right()),
        };
        if crosses {
            let ny =
                (next.y.floor() as i64).clamp(i64::from(r.top()), i64::from(r.bottom() - 1)) as i32;
            let nx = match self.facing() {
                Side::Left => r.left() - 1,
                Side::Right => r.right(),
            };
            if !self.peer.contains(Point::new(nx, ny)) && !buttons_held {
                return Step::Exited {
                    warp_to: self.leave_point(next),
                };
            }
        }
        Step::Moved(self.peer.clamp_f(next))
    }

    /// Where my cursor reappears after the virtual cursor left the peer at
    /// `v_out`: just inside my free P edge, never on it.
    pub fn leave_point(&self, v_out: PointF) -> Point {
        let y_l = self.map_y_to_local(v_out.y);
        let (i, y) = self.local.face(y_l, self.peer_side);
        let r = self.local.screens()[i].rect;
        let x = match self.peer_side {
            Side::Right => r.right() - 1 - EDGE_REENTRY_INSET,
            Side::Left => r.left() + EDGE_REENTRY_INSET,
        };
        r.clamp(Point::new(x, y.floor() as i32))
    }

    /// `exit` moved `EDGE_REENTRY_INSET` away from P, clamped into its
    /// screen: the warp target when a grab ends for any reason other than
    /// crossing back.
    pub fn inset(&self, exit: Point) -> Point {
        let x = match self.peer_side {
            Side::Right => exit.x.saturating_sub(EDGE_REENTRY_INSET),
            Side::Left => exit.x.saturating_add(EDGE_REENTRY_INSET),
        };
        let exit = self.local.clamp(exit);
        let r = self.local.screen_for(exit.to_f()).rect;
        r.clamp(Point::new(x, exit.y))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::geometry::tests::screen;
    use crate::types::ScreenInfo;

    fn mac() -> ScreenSet {
        ScreenSet::new(vec![screen(1, 0, 0, 1440, 900, 1.0)]).unwrap()
    }

    fn win(npl: f64) -> ScreenSet {
        ScreenSet::new(vec![screen(7, 0, 0, 1920, 1080, npl)]).unwrap()
    }

    #[test]
    fn enter_right_and_left() {
        let a = Layout::new(mac(), win(1.0), Side::Right);
        let c = a.try_enter(PointF::new(1439.5, 450.0)).unwrap();
        assert_eq!(c.entry, PointF::new(0.0, 540.0));
        assert_eq!(c.exit, Point::new(1439, 450));
        assert_eq!(c.npl_src, 1.0);
        // Unclamped OS positions past the edge are clamped first.
        assert_eq!(
            a.try_enter(PointF::new(1500.0, 450.0)).unwrap().exit,
            Point::new(1439, 450)
        );
        assert!(a.try_enter(PointF::new(1438.0, 450.0)).is_none());

        let b = Layout::new(win(1.0), mac(), Side::Left);
        let c = b.try_enter(PointF::new(0.0, 540.0)).unwrap();
        assert_eq!(c.entry, PointF::new(1439.0, 450.0));
        assert_eq!(c.exit, Point::new(0, 540));
        assert!(b.try_enter(PointF::new(1919.0, 540.0)).is_none());
    }

    #[test]
    fn step_moves_normalises_and_exits() {
        // Windows at 1.5 controlling a Mac at 1.0: 3 px -> 2 points.
        let l = Layout::new(win(1.5), mac(), Side::Left);
        let v = PointF::new(1439.0, 450.0);
        assert_eq!(
            l.step(v, PointF::new(-3.0, 0.0), 1.5, false),
            Step::Moved(PointF::new(1437.0, 450.0))
        );
        // Crossing back over the facing (right) edge of the Mac.
        match l.step(v, PointF::new(3.0, 0.0), 1.5, false) {
            Step::Exited { warp_to } => assert_eq!(warp_to, Point::new(2, 540)),
            other => panic!("expected exit, got {other:?}"),
        }
        // Held button: clamped instead of leaving.
        assert_eq!(
            l.step(v, PointF::new(3.0, 0.0), 1.5, true),
            Step::Moved(PointF::new(1439.0, 450.0))
        );
        // Other edges clamp.
        assert_eq!(
            l.step(
                PointF::new(10.0, 5.0),
                PointF::new(-100.0, -100.0),
                1.5,
                false
            ),
            Step::Moved(PointF::new(0.0, 0.0))
        );
    }

    #[test]
    fn leave_point_and_inset() {
        let a = Layout::new(mac(), win(1.0), Side::Right);
        assert_eq!(
            a.leave_point(PointF::new(-1.0, 540.0)),
            Point::new(1437, 450)
        );
        assert_eq!(a.inset(Point::new(1439, 450)), Point::new(1437, 450));
        let b = Layout::new(win(1.0), mac(), Side::Left);
        assert_eq!(b.inset(Point::new(0, 540)), Point::new(2, 540));
        assert_eq!(
            b.leave_point(PointF::new(1440.0, 450.0)),
            Point::new(2, 540)
        );
    }

    #[test]
    fn multi_monitor_free_edge_entry() {
        // Laptop with an external monitor on its right, offset down. Peer on the right.
        let local = ScreenSet::new(vec![
            screen(1, 0, 0, 1440, 900, 1.0),
            screen(2, 1440, 300, 1920, 1080, 1.0),
        ])
        .unwrap();
        let l = Layout::new(local, win(1.0), Side::Right);
        // Above the external monitor the laptop's right edge is free.
        assert!(l.try_enter(PointF::new(1439.0, 100.0)).is_some());
        assert!(l.try_enter(PointF::new(1439.0, 500.0)).is_none());
        assert!(l.try_enter(PointF::new(3359.0, 500.0)).is_some());
    }

    // ---- property tests (design §5.3) ----

    /// 1-4 screens tiled on a coarse grid (cells 400x300, sizes 100..=400 x
    /// 100..=300), unique ids, random scales.
    fn arb_screens() -> impl Strategy<Value = ScreenSet> {
        prop::collection::btree_set((0i32..4, 0i32..3), 1..=4)
            .prop_flat_map(|cells| {
                let n = cells.len();
                (
                    Just(cells),
                    prop::collection::vec(
                        (
                            100i32..=400,
                            100i32..=300,
                            prop::sample::select(vec![1.0, 1.25, 1.5, 2.0]),
                        ),
                        n,
                    ),
                )
            })
            .prop_map(|(cells, dims)| {
                let screens: Vec<ScreenInfo> = cells
                    .into_iter()
                    .zip(dims)
                    .enumerate()
                    .map(|(i, ((cx, cy), (w, h, npl)))| {
                        screen(i as u32 + 1, cx * 400, cy * 300, w, h, npl)
                    })
                    .collect();
                ScreenSet::new(screens).unwrap()
            })
    }

    fn arb_side() -> impl Strategy<Value = Side> {
        prop_oneof![Just(Side::Left), Just(Side::Right)]
    }

    fn arb_point() -> impl Strategy<Value = PointF> {
        (-2000.0f64..3000.0, -2000.0f64..3000.0).prop_map(|(x, y)| PointF::new(x, y))
    }

    proptest! {
        #[test]
        fn prop_entry_on_free_facing_edge(local in arb_screens(), peer in arb_screens(), side in arb_side(), y in -500.0f64..1500.0) {
            let l = Layout::new(local.clone(), peer.clone(), side);
            // Push against every local screen's P edge at height y.
            for s in local.screens() {
                let r = s.rect;
                let x = match side { Side::Right => f64::from(r.right() - 1), Side::Left => f64::from(r.left()) };
                if let Some(c) = l.try_enter(PointF::new(x, y)) {
                    prop_assert!(peer.contains_f(c.entry), "entry {:?} off screen", c.entry);
                    prop_assert!(peer.at_free_edge(c.entry, side.opposite()), "entry {:?} not on free edge", c.entry);
                    prop_assert!(local.contains(c.exit));
                }
            }
        }

        #[test]
        fn prop_leave_point_never_on_free_edge(local in arb_screens(), peer in arb_screens(), side in arb_side(), v in arb_point()) {
            let l = Layout::new(local.clone(), peer, side);
            let p = l.leave_point(v);
            prop_assert!(local.contains(p), "leave point {:?} off screen", p);
            prop_assert!(!local.at_free_edge(p.to_f(), side), "leave point {:?} on free edge", p);
        }

        #[test]
        fn prop_y_mapping_monotonic(local in arb_screens(), peer in arb_screens(), side in arb_side(), a in -500.0f64..1500.0, b in -500.0f64..1500.0) {
            let l = Layout::new(local, peer, side);
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(l.map_y_to_peer(lo) <= l.map_y_to_peer(hi));
            prop_assert!(l.map_y_to_local(lo) <= l.map_y_to_local(hi));
        }

        #[test]
        fn prop_single_screen_round_trip(w1 in 100i32..4000, h1 in 100i32..3000, w2 in 100i32..4000, h2 in 100i32..3000, side in arb_side(), frac in 0.0f64..1.0) {
            let local = ScreenSet::new(vec![screen(1, 0, 0, w1, h1, 1.0)]).unwrap();
            let peer = ScreenSet::new(vec![screen(2, 0, 0, w2, h2, 1.0)]).unwrap();
            let l = Layout::new(local, peer, side);
            let y = frac * f64::from(h1 - 1);
            let x = match side { Side::Right => f64::from(w1 - 1), Side::Left => 0.0 };
            let c = l.try_enter(PointF::new(x, y)).expect("single screen edge is always free");
            // Step straight back out over the facing edge.
            let dx = match side { Side::Right => -5.0, Side::Left => 5.0 };
            // The mapping is exact except where the entry is clamped into the
            // peer's last row, which costs at most one peer row in local units.
            let tolerance = 1.0 + f64::from(h1) / f64::from(h2);
            match l.step(c.entry, PointF::new(dx, 0.0), 1.0, false) {
                Step::Exited { warp_to } => prop_assert!((f64::from(warp_to.y) - y).abs() <= tolerance, "{} vs {}", warp_to.y, y),
                other => prop_assert!(false, "expected exit, got {:?}", other),
            }
            // Away from the bottom row the round trip is within one unit.
            if y * f64::from(h2) / f64::from(h1) <= f64::from(h2 - 1)
                && let Step::Exited { warp_to } = l.step(c.entry, PointF::new(dx, 0.0), 1.0, false)
            {
                prop_assert!((f64::from(warp_to.y) - y).abs() <= 1.0, "{} vs {}", warp_to.y, y);
            }
        }

        #[test]
        fn prop_clamp_idempotent_and_on_screen(s in arb_screens(), p in arb_point()) {
            let c = s.clamp_f(p);
            prop_assert!(s.contains_f(c));
            prop_assert_eq!(s.clamp_f(c), c);
            let ci = s.clamp(p.floor());
            prop_assert!(s.contains(ci));
            prop_assert_eq!(s.clamp(ci), ci);
        }

        #[test]
        fn prop_normalisation_identity_for_equal_scales(npl in prop::sample::select(vec![1.0, 1.25, 1.5, 2.0]), dx in -50.0f64..50.0, dy in -50.0f64..50.0) {
            let local = ScreenSet::new(vec![screen(1, 0, 0, 1000, 1000, npl)]).unwrap();
            let peer = ScreenSet::new(vec![screen(2, 0, 0, 1000, 1000, npl)]).unwrap();
            let l = Layout::new(local, peer, Side::Right);
            let v = PointF::new(500.0, 500.0);
            prop_assert_eq!(l.step(v, PointF::new(dx, dy), npl, false), Step::Moved(PointF::new(500.0 + dx, 500.0 + dy)));
        }
    }
}
