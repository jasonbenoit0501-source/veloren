//! What building blocks cost, and a record of what was paid for them.
//!
//! Players building in a claimed plot (see [`crate::plots`]) pay for each block
//! they place with ordinary materials, and get the materials back when they
//! remove a block that was paid for. Admins, and build areas handed out with
//! `/permit_build`, keep building for free.
//!
//! [`BuildLedger`] remembers which blocks were paid for and with what, so that
//! removing natural terrain never gives out free materials. It is saved to
//! [`BUILD_LEDGER_FILENAME`] in the server data directory.

use atomicwrites::{AtomicFile, OverwriteBehavior};
use common::terrain::{Block, BlockKind, SpriteKind};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tracing::{error, info};
use vek::*;

pub const BUILD_LEDGER_FILENAME: &str = "build_ledger.ron";

/// How often new ledger entries are written to disk. This matches how often
/// terrain changes are saved.
const SAVE_INTERVAL: Duration = Duration::from_secs(60);

/// The materials needed to place a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildCost {
    /// The item asset that is used up.
    pub item: &'static str,
    pub amount: u32,
    /// A name for the item, used in messages to the player.
    pub name: &'static str,
}

const WOOD: BuildCost = BuildCost {
    item: "common.items.log.wood",
    amount: 1,
    name: "Wood Log",
};
const STONES: BuildCost = BuildCost {
    item: "common.items.crafting_ing.stones",
    amount: 1,
    name: "Stones",
};
const TWIGS: BuildCost = BuildCost {
    item: "common.items.crafting_ing.twigs",
    amount: 1,
    name: "Twigs",
};

/// What placing this block costs, or `None` if players can't build with it.
///
/// Only plain solid blocks are buildable: water, lava, glowing blocks and
/// blocks that can be mined for items (weak rock, ice) are not, so placing and
/// then mining a block can never produce items.
pub fn building_cost(block: Block) -> Option<BuildCost> {
    match block.kind() {
        BlockKind::Wood => Some(WOOD),
        BlockKind::Rock => Some(STONES),
        BlockKind::Leaves | BlockKind::ArtLeaves => Some(TWIGS),
        // The default building block, a plain coloured block
        BlockKind::Misc => Some(STONES),
        _ => None,
    }
}

/// Whether a paying player may place a block where `block` currently is.
///
/// Only empty space (air or water) can be built in, so placing can't destroy
/// other blocks, chests, crops or anything else worth keeping. Small
/// decorations such as grass are replaced.
pub fn is_replaceable(block: Block) -> bool {
    block.is_fluid()
        && block.get_sprite().is_none_or(|sprite| {
            sprite == SpriteKind::Empty
                || (sprite.solid_height().is_none()
                    && !sprite.is_defined_as_container()
                    && !block.is_collectible()
                    && block.mine_tool().is_none())
        })
}

/// What was paid for a placed block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payment {
    pub item: String,
    pub amount: u32,
}

#[derive(Serialize, Deserialize)]
struct SavedEntry {
    pos: [i32; 3],
    /// The block that was placed, see [`Block::to_u32`].
    block: u32,
    payment: Payment,
}

/// A record of the blocks players paid for, so they can be refunded.
pub struct BuildLedger {
    path: PathBuf,
    entries: HashMap<Vec3<i32>, (u32, Payment)>,
    modified: bool,
    /// False if the existing file couldn't be read, so that it is never
    /// overwritten.
    can_save: bool,
    last_save: Instant,
}

impl BuildLedger {
    /// Load the ledger from the data directory. A file that cannot be parsed is
    /// moved aside to a backup rather than being overwritten.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(BUILD_LEDGER_FILENAME);
        let mut can_save = true;
        let entries = match std::fs::read_to_string(&path) {
            Ok(contents) => match ron::from_str::<Vec<SavedEntry>>(&contents) {
                Ok(saved) => saved
                    .into_iter()
                    .map(|e| (Vec3::from(e.pos), (e.block, e.payment)))
                    .collect(),
                Err(error) => {
                    let backup_path = backup_path(&path);
                    error!(
                        ?error,
                        "Failed to parse {:?}, moving it to {:?}. Blocks placed before now won't \
                         be refunded.",
                        path,
                        backup_path
                    );
                    if let Err(error) = std::fs::rename(&path, &backup_path) {
                        error!(?error, "Failed to back up unreadable build ledger");
                        can_save = false;
                    }
                    HashMap::new()
                },
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(error) => {
                error!(
                    ?error,
                    ?path,
                    "Failed to read build ledger, it will not be saved"
                );
                can_save = false;
                HashMap::new()
            },
        };
        info!(?path, blocks = entries.len(), "Loaded build ledger");
        Self {
            path,
            entries,
            modified: false,
            can_save,
            last_save: Instant::now(),
        }
    }

    /// Record that `block` was placed at `pos` and paid for with `payment`.
    pub fn placed(&mut self, pos: Vec3<i32>, block: Block, payment: Payment) {
        self.entries.insert(pos, (block.to_u32(), payment));
        self.modified = true;
    }

    /// What removing `block` at `pos` should give back: only blocks that are
    /// still exactly as they were paid for are refunded.
    pub fn refund_for(&self, pos: Vec3<i32>, block: Block) -> Option<&Payment> {
        self.entries
            .get(&pos)
            .filter(|(placed, _)| *placed == block.to_u32())
            .map(|(_, payment)| payment)
    }

    /// Forget the payment for the block at `pos`, once it has been refunded.
    pub fn removed(&mut self, pos: Vec3<i32>) {
        if self.entries.remove(&pos).is_some() {
            self.modified = true;
        }
    }

    /// Periodically write changes to disk.
    pub fn maintain(&mut self) {
        if self.last_save.elapsed() >= SAVE_INTERVAL {
            self.save();
        }
    }

    pub fn save(&mut self) {
        self.last_save = Instant::now();
        if !self.modified || !self.can_save {
            return;
        }
        let saved = self
            .entries
            .iter()
            .map(|(pos, (block, payment))| SavedEntry {
                pos: pos.into_array(),
                block: *block,
                payment: payment.clone(),
            })
            .collect::<Vec<_>>();
        let contents = match ron::to_string(&saved) {
            Ok(contents) => contents,
            Err(error) => {
                error!(?error, "Failed to serialize build ledger");
                return;
            },
        };
        let file = AtomicFile::new(&self.path, OverwriteBehavior::AllowOverwrite);
        match file.write(|f| f.write_all(contents.as_bytes())) {
            Ok(()) => self.modified = false,
            Err(error) => error!(?error, path = ?self.path, "Failed to save build ledger"),
        }
    }
}

impl Drop for BuildLedger {
    fn drop(&mut self) { self.save(); }
}

/// Find an unused path to move an unreadable file to.
fn backup_path(path: &Path) -> PathBuf {
    (0..)
        .map(|i| path.with_extension(format!("ron_backup_{i}")))
        .find(|p| !p.exists())
        .expect("There is always an unused backup name")
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{assets::AssetExt, comp::item::ItemDef};
    use std::sync::Arc;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "veloren-building-costs-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn block(kind: BlockKind) -> Block { Block::new(kind, Rgb::new(10, 20, 30)) }

    #[test]
    fn costs_use_real_items() {
        for kind in [
            BlockKind::Wood,
            BlockKind::Rock,
            BlockKind::Leaves,
            BlockKind::Misc,
        ] {
            let cost = building_cost(block(kind)).unwrap();
            assert!(
                Arc::<ItemDef>::load_cloned(cost.item).is_ok(),
                "{}",
                cost.item
            );
            assert_eq!(cost.amount, 1);
        }
        for kind in [
            BlockKind::WeakRock,
            BlockKind::GlowingWeakRock,
            BlockKind::Ice,
            BlockKind::Lava,
            BlockKind::Water,
            BlockKind::Air,
            BlockKind::GlowingRock,
        ] {
            assert_eq!(building_cost(block(kind)), None, "{kind:?}");
        }
    }

    #[test]
    fn only_empty_space_is_replaceable() {
        assert!(is_replaceable(Block::empty()));
        assert!(is_replaceable(Block::water(SpriteKind::Empty)));
        assert!(!is_replaceable(block(BlockKind::Rock)));
        assert!(!is_replaceable(Block::air(SpriteKind::Chest)));
        assert!(!is_replaceable(Block::air(SpriteKind::Apple)));
    }

    #[test]
    fn ledger_survives_restart() {
        let dir = test_dir("restart");
        let pos = Vec3::new(-3, 7, 100);
        let wood = block(BlockKind::Wood);
        let payment = Payment {
            item: "common.items.log.wood".to_owned(),
            amount: 1,
        };
        {
            let mut ledger = BuildLedger::load(&dir);
            ledger.placed(pos, wood, payment.clone());
            ledger.placed(Vec3::new(0, 0, 0), wood, payment.clone());
            ledger.removed(Vec3::new(0, 0, 0));
        }

        let ledger = BuildLedger::load(&dir);
        assert_eq!(ledger.refund_for(pos, wood), Some(&payment));
        // A different block in the same place isn't refunded
        assert_eq!(ledger.refund_for(pos, block(BlockKind::Rock)), None);
        assert_eq!(ledger.refund_for(Vec3::new(0, 0, 0), wood), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_ledger_is_backed_up() {
        let dir = test_dir("corrupt");
        std::fs::write(dir.join(BUILD_LEDGER_FILENAME), "not a ledger").unwrap();
        let mut ledger = BuildLedger::load(&dir);
        ledger.placed(Vec3::zero(), block(BlockKind::Wood), Payment {
            item: "common.items.log.wood".to_owned(),
            amount: 1,
        });
        ledger.save();
        assert_eq!(
            std::fs::read_to_string(dir.join("build_ledger.ron_backup_0")).unwrap(),
            "not a ledger"
        );
        assert!(dir.join(BUILD_LEDGER_FILENAME).is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
