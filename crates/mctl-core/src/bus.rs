//! Lock-free publication of the latest [`World`]: producers never block on the reader, and the
//! reader never blocks on producers (DESIGN.md §6.2).

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::world::World;

pub struct Bus {
    slot: ArcSwap<World>,
}

impl Bus {
    pub fn new(initial: World) -> Arc<Self> {
        Arc::new(Self {
            slot: ArcSwap::from_pointee(initial),
        })
    }

    pub fn publish(&self, world: World) {
        self.slot.store(Arc::new(world));
    }

    pub fn latest(&self) -> Arc<World> {
        self.slot.load_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Utc;

    #[test]
    fn readers_see_the_latest_publication_across_threads() {
        let bus = Bus::new(World::empty(Utc(0.0)));
        let b2 = bus.clone();
        let h = std::thread::spawn(move || {
            for seq in 1..=100 {
                let mut w = World::empty(Utc(seq as f64));
                w.seq = seq;
                b2.publish(w);
            }
        });
        h.join().unwrap();
        assert_eq!(bus.latest().seq, 100);
    }
}
