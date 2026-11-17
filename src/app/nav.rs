//! The history of the places that the person visits, for the back and the forward parts of the
//! header, as a browser keeps it. A place is a screen, and for the live view of a ticket, the
//! pagers on top of it. The loader is no place.
//!
//! The update notes the place after each message. When a key or a click goes to another place,
//! the place before it goes on the back list, and the forward list empties. Back takes the last
//! place of the back list, and the place that showed goes on the forward list. Forward does the
//! reverse.
//!
//! A load can change the place too: a pager closes when its text does not exist, such as the
//! plan of a ticket without a plan. Such a change only corrects the place that the history
//! holds. When it goes back to the last place of the back list, the visit that put that place
//! there ends, so that back does not show the same place again.

use super::pager::PagerKind;

/// The most places that each list keeps. The oldest place goes first.
pub const KEEP: usize = 50;

/// A place of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Home,
    /// The live view of a ticket, with the kinds of its pagers, the bottom one first.
    Live {
        key: String,
        pagers: Vec<PagerKind>,
    },
    Models,
    Stats,
}

impl Place {
    /// The steps of the place from Home: 0 for Home, 1 for a screen, and one more for each
    /// pager.
    pub fn depth(&self) -> usize {
        match self {
            Place::Home => 0,
            Place::Live { pagers, .. } => 1 + pagers.len(),
            Place::Models | Place::Stats => 1,
        }
    }

    /// The place `depth` steps from Home on the way to this place: 0 is Home, 1 is the screen
    /// without a pager, and each step after it keeps one more pager. A depth of this place or
    /// more gives this place.
    pub fn up_to(&self, depth: usize) -> Place {
        if depth == 0 {
            return Place::Home;
        }
        match self {
            Place::Live { key, pagers } => Place::Live {
                key: key.clone(),
                pagers: pagers[..(depth - 1).min(pagers.len())].to_vec(),
            },
            other => other.clone(),
        }
    }
}

/// The history.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Nav {
    /// The place that shows, as the history knows it.
    pub at: Option<Place>,
    /// The places before it, the last one last.
    pub back: Vec<Place>,
    /// The places that back left, the next one last.
    pub forward: Vec<Place>,
}

impl Nav {
    /// Notes the place that shows after a key or a click. Another place puts the place before
    /// it on the back list and empties the forward list. The loader, `None`, changes nothing.
    pub fn visit(&mut self, now: Option<Place>) {
        let Some(now) = now else {
            return;
        };
        if self.at.as_ref() == Some(&now) {
            return;
        }
        if let Some(old) = self.at.replace(now) {
            keep(&mut self.back, old);
            self.forward.clear();
        }
    }

    /// Notes the place that shows after a load. The place that the history holds changes, and
    /// no list gets a place. A return to the last place of the back list ends the visit that
    /// put it there.
    pub fn correct(&mut self, now: Option<Place>) {
        let Some(now) = now else {
            return;
        };
        if self.at.as_ref() == Some(&now) {
            return;
        }
        if self.back.last() == Some(&now) {
            self.back.pop();
        }
        self.at = Some(now);
    }

    /// Back: takes the last place of the back list, which becomes the place that shows. The
    /// place that showed goes on the forward list.
    pub fn back(&mut self) -> Option<Place> {
        let target = self.back.pop()?;
        if let Some(at) = self.at.replace(target.clone()) {
            keep(&mut self.forward, at);
        }
        Some(target)
    }

    /// Forward: takes the last place of the forward list, which becomes the place that shows.
    /// The place that showed goes on the back list.
    pub fn forward(&mut self) -> Option<Place> {
        let target = self.forward.pop()?;
        if let Some(at) = self.at.replace(target.clone()) {
            keep(&mut self.back, at);
        }
        Some(target)
    }
}

/// Puts a place at the end of a list, and drops the oldest place over [`KEEP`].
fn keep(list: &mut Vec<Place>, place: Place) {
    list.push(place);
    if list.len() > KEEP {
        list.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(key: &str, pagers: &[PagerKind]) -> Place {
        Place::Live {
            key: key.into(),
            pagers: pagers.to_vec(),
        }
    }

    #[test]
    fn a_visit_goes_on_the_back_list_and_back_and_forward_walk_the_lists() {
        let mut nav = Nav::default();
        nav.visit(None);
        assert_eq!(nav, Nav::default(), "the loader is no place");
        nav.visit(Some(Place::Home));
        nav.visit(Some(Place::Home));
        assert!(nav.back.is_empty(), "the same place is no visit");
        nav.visit(Some(live("DEMO-12", &[])));
        nav.visit(Some(live("DEMO-12", &[PagerKind::Log])));
        assert_eq!(nav.back, [Place::Home, live("DEMO-12", &[])]);
        assert_eq!(nav.back(), Some(live("DEMO-12", &[])));
        assert_eq!(nav.back(), Some(Place::Home));
        assert_eq!(nav.back(), None, "the back list is empty");
        assert_eq!(nav.at, Some(Place::Home));
        assert_eq!(
            nav.forward,
            [live("DEMO-12", &[PagerKind::Log]), live("DEMO-12", &[])]
        );
        assert_eq!(nav.forward(), Some(live("DEMO-12", &[])));
        assert_eq!(nav.back, [Place::Home]);
        // A visit empties the forward list.
        nav.visit(Some(Place::Models));
        assert!(nav.forward.is_empty());
        assert_eq!(nav.back, [Place::Home, live("DEMO-12", &[])]);
        assert_eq!(nav.forward(), None);
    }

    #[test]
    fn a_load_that_closes_a_pager_ends_its_visit() {
        let mut nav = Nav::default();
        nav.visit(Some(live("DEMO-12", &[])));
        nav.visit(Some(live("DEMO-12", &[PagerKind::Plan])));
        // The ticket has no plan, so the pager closes.
        nav.correct(Some(live("DEMO-12", &[])));
        assert!(nav.back.is_empty(), "back does not show the same place");
        assert_eq!(nav.at, Some(live("DEMO-12", &[])));
        // Another change only corrects the place.
        nav.visit(Some(Place::Home));
        nav.correct(Some(Place::Stats));
        assert_eq!(nav.back, [live("DEMO-12", &[])]);
        assert_eq!(nav.at, Some(Place::Stats));
    }

    #[test]
    fn each_list_keeps_the_newest_places() {
        let mut nav = Nav::default();
        for i in 0..=KEEP + 5 {
            nav.visit(Some(live(&format!("T-{i}"), &[])));
        }
        assert_eq!(nav.back.len(), KEEP);
        assert_eq!(nav.back[0], live("T-5", &[]));
    }

    #[test]
    fn a_place_up_to_a_depth_keeps_the_way_from_home() {
        let turn = PagerKind::Turn { id: "t1".into() };
        let deep = live("DEMO-12", &[PagerKind::Log, turn.clone()]);
        assert_eq!(deep.depth(), 3);
        assert_eq!(deep.up_to(0), Place::Home);
        assert_eq!(deep.up_to(1), live("DEMO-12", &[]));
        assert_eq!(deep.up_to(2), live("DEMO-12", &[PagerKind::Log]));
        assert_eq!(deep.up_to(3), deep);
        assert_eq!(deep.up_to(9), deep);
        assert_eq!(Place::Models.up_to(1), Place::Models);
        assert_eq!(Place::Stats.up_to(0), Place::Home);
        assert_eq!((Place::Home.depth(), Place::Models.depth()), (0, 1));
    }
}
