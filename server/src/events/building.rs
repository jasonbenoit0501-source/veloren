//! Paid building inside claimed plots.
//!
//! Build mode messages from players building in a plot they may build in are
//! turned into [`PlotBuildEvent`]s, so the materials can be taken from, or
//! given back to, the player's inventory (see [`crate::building_costs`]).

use super::{ServerEvent, event_dispatch};
use crate::{
    building_costs::{BuildLedger, Payment, building_cost, is_replaceable},
    client::Client,
    sys::terrain::TerrainPersistenceData,
};
use common::{
    assets::AssetExt,
    comp::{
        ChatType, Content, Inventory,
        item::{Item, ItemDef, MaterialStatManifest, tool::AbilityMap},
    },
    terrain::{Block, TerrainGrid},
    vol::ReadVol,
};
use common_net::msg::ServerGeneral;
use common_state::BlockChange;
use specs::{DispatcherBuilder, Entity as EcsEntity, ReadExpect, ReadStorage, Write, WriteStorage};
use std::sync::Arc;
use tracing::error;
use vek::*;

/// A player placing or removing a block in a plot.
pub struct PlotBuildEvent {
    pub entity: EcsEntity,
    pub pos: Vec3<i32>,
    pub action: PlotBuildAction,
}

pub enum PlotBuildAction {
    Place(Block),
    Remove,
}

pub(super) fn register_event_systems(builder: &mut DispatcherBuilder) {
    event_dispatch::<PlotBuildEvent>(builder, &[]);
}

impl ServerEvent for PlotBuildEvent {
    type SystemData<'a> = (
        Write<'a, BlockChange>,
        ReadExpect<'a, TerrainGrid>,
        TerrainPersistenceData<'a>,
        Option<Write<'a, BuildLedger>>,
        WriteStorage<'a, Inventory>,
        ReadStorage<'a, Client>,
        ReadExpect<'a, AbilityMap>,
        ReadExpect<'a, MaterialStatManifest>,
    );

    fn handle(
        events: impl ExactSizeIterator<Item = Self>,
        (
            mut block_change,
            terrain,
            mut _terrain_persistence,
            mut ledger,
            mut inventories,
            clients,
            ability_map,
            msm,
        ): Self::SystemData<'_>,
    ) {
        for ev in events {
            let notify = |msg: String| {
                if let Some(client) = clients.get(ev.entity) {
                    client.send_fallible(ServerGeneral::server_msg(
                        ChatType::CommandError,
                        Content::Plain(msg),
                    ));
                }
            };
            let Ok(old_block) = terrain.get(ev.pos).copied() else {
                continue;
            };
            let Some(mut inventory) = inventories.get_mut(ev.entity) else {
                continue;
            };

            let new_block = match ev.action {
                PlotBuildAction::Place(new_block) => {
                    let Some(cost) = building_cost(new_block) else {
                        notify(
                            "You can only build with wood, rock, leaf or plain blocks.".to_owned(),
                        );
                        continue;
                    };
                    if !is_replaceable(old_block) {
                        continue;
                    }
                    let Ok(item_def) = Arc::<ItemDef>::load_cloned(cost.item) else {
                        error!(item = cost.item, "Failed to load building material");
                        continue;
                    };
                    if inventory.item_count(&item_def) < u64::from(cost.amount) {
                        notify(format!(
                            "You need {} {} to place this block.",
                            cost.amount, cost.name
                        ));
                        continue;
                    }
                    // Only pay once the block has actually been placed
                    if block_change.try_set(ev.pos, new_block).is_none() {
                        continue;
                    }
                    if inventory
                        .remove_item_amount(&item_def, cost.amount, &ability_map, &msm)
                        .is_none()
                    {
                        error!("Building materials disappeared after they were counted");
                    }
                    if let Some(ledger) = ledger.as_mut() {
                        ledger.placed(ev.pos, new_block, Payment {
                            item: cost.item.to_owned(),
                            amount: cost.amount,
                        });
                    }
                    new_block
                },
                PlotBuildAction::Remove => {
                    let new_block = old_block.into_vacant();
                    let refund = ledger
                        .as_ref()
                        .and_then(|ledger| ledger.refund_for(ev.pos, old_block))
                        .and_then(|payment| {
                            let item_def = Arc::<ItemDef>::load_cloned(&payment.item).ok()?;
                            let mut item = Item::new_from_asset(&payment.item).ok()?;
                            item.set_amount(payment.amount).ok()?;
                            Some((item_def, item))
                        });
                    // Never remove a paid-for block if its materials can't be given back
                    if let Some((item_def, item)) = &refund
                        && !inventory.has_space_for(item_def, item.amount())
                    {
                        notify(
                            "Your inventory is too full to take back this block's materials."
                                .to_owned(),
                        );
                        continue;
                    }
                    if block_change.try_set(ev.pos, new_block).is_none() {
                        continue;
                    }
                    if let Some((_, item)) = refund {
                        if let Some(ledger) = ledger.as_mut() {
                            ledger.removed(ev.pos);
                        }
                        if let Err((item, _)) = inventory.push(item) {
                            error!(?item, "Failed to refund building materials");
                        }
                    }
                    new_block
                },
            };

            #[cfg(feature = "persistent_world")]
            if let Some(terrain_persistence) = _terrain_persistence.as_mut() {
                terrain_persistence.set_block(ev.pos, new_block);
            }
            #[cfg(not(feature = "persistent_world"))]
            let _ = new_block;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::building_costs::BuildLedger;
    use common::{
        terrain::{BlockKind, MapSizeLg, TerrainChunk, TerrainChunkMeta},
        vol::WriteVol,
    };
    use specs::{Builder, World, WorldExt};
    use std::path::{Path, PathBuf};

    const WOOD_LOG: &str = "common.items.log.wood";

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("veloren-building-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wood() -> Block { Block::new(BlockKind::Wood, Rgb::new(120, 80, 40)) }

    fn rock() -> Block { Block::new(BlockKind::Rock, Rgb::new(100, 100, 100)) }

    fn items(id: &str, amount: u32) -> Item {
        let mut item = Item::new_from_asset_expect(id);
        item.set_amount(amount).unwrap();
        item
    }

    fn count(world: &World, entity: EcsEntity, id: &str) -> u64 {
        world
            .read_storage::<Inventory>()
            .get(entity)
            .unwrap()
            .item_count(&Arc::<ItemDef>::load_expect_cloned(id))
    }

    /// Rock below z = 0, air above, with a wood block at (5, 5, 5).
    fn test_world(dir: &Path) -> World {
        let mut world = World::new();
        world.register::<Inventory>();
        world.register::<Client>();
        world.insert(MaterialStatManifest::load().cloned());
        world.insert(AbilityMap::load().cloned());
        world.insert(BlockChange::default());
        world.insert(BuildLedger::load(dir));

        let mut chunk = TerrainChunk::new(0, rock(), Block::empty(), TerrainChunkMeta::void());
        chunk.set(Vec3::new(5, 5, 5), wood()).unwrap();
        let mut terrain = TerrainGrid::new(
            MapSizeLg::new(Vec2::new(5, 5)).unwrap(),
            Arc::new(chunk.clone()),
        )
        .unwrap();
        terrain.insert(Vec2::new(0, 0), Arc::new(chunk));
        world.insert(terrain);
        world
    }

    fn player(world: &mut World, inventory_items: Vec<Item>) -> EcsEntity {
        let mut inventory = Inventory::with_empty();
        for item in inventory_items {
            inventory.push(item).unwrap();
        }
        world.create_entity().with(inventory).build()
    }

    fn run(world: &mut World, entity: EcsEntity, pos: Vec3<i32>, action: PlotBuildAction) {
        world.write_resource::<BlockChange>().clear();
        PlotBuildEvent::handle(
            [PlotBuildEvent {
                entity,
                pos,
                action,
            }]
            .into_iter(),
            world.system_data(),
        );
    }

    fn was_changed(world: &World, pos: Vec3<i32>) -> bool {
        !world.read_resource::<BlockChange>().can_set_block(pos)
    }

    #[test]
    fn placing_costs_materials() {
        let dir = test_dir("place");
        let mut world = test_world(&dir);
        let builder = player(&mut world, vec![items(WOOD_LOG, 2)]);
        let pos = Vec3::new(1, 1, 3);

        run(&mut world, builder, pos, PlotBuildAction::Place(wood()));
        assert!(was_changed(&world, pos));
        assert_eq!(count(&world, builder, WOOD_LOG), 1);
        assert!(
            world
                .read_resource::<BuildLedger>()
                .refund_for(pos, wood())
                .is_some()
        );

        // Can't build inside solid terrain, and nothing is taken
        run(
            &mut world,
            builder,
            Vec3::new(1, 1, -3),
            PlotBuildAction::Place(wood()),
        );
        assert!(!was_changed(&world, Vec3::new(1, 1, -3)));
        assert_eq!(count(&world, builder, WOOD_LOG), 1);

        // Blocks that could be mined for items can't be placed
        let weak_rock = Block::new(BlockKind::WeakRock, Rgb::new(100, 100, 100));
        run(&mut world, builder, pos, PlotBuildAction::Place(weak_rock));
        assert!(!was_changed(&world, pos));
        assert_eq!(count(&world, builder, WOOD_LOG), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn placing_without_materials_does_nothing() {
        let dir = test_dir("broke");
        let mut world = test_world(&dir);
        let builder = player(&mut world, vec![items(
            "common.items.crafting_ing.stones",
            3,
        )]);
        let pos = Vec3::new(2, 2, 3);

        run(&mut world, builder, pos, PlotBuildAction::Place(wood()));
        assert!(!was_changed(&world, pos));
        assert_eq!(
            count(&world, builder, "common.items.crafting_ing.stones"),
            3
        );

        // But stones are enough for a rock block
        run(&mut world, builder, pos, PlotBuildAction::Place(rock()));
        assert!(was_changed(&world, pos));
        assert_eq!(
            count(&world, builder, "common.items.crafting_ing.stones"),
            2
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_refunds_only_paid_blocks() {
        let dir = test_dir("refund");
        let mut world = test_world(&dir);
        let builder = player(&mut world, vec![]);
        let placed = Vec3::new(5, 5, 5);
        world
            .write_resource::<BuildLedger>()
            .placed(placed, wood(), Payment {
                item: WOOD_LOG.to_owned(),
                amount: 1,
            });

        run(&mut world, builder, placed, PlotBuildAction::Remove);
        assert!(was_changed(&world, placed));
        assert_eq!(count(&world, builder, WOOD_LOG), 1);
        assert!(
            world
                .read_resource::<BuildLedger>()
                .refund_for(placed, wood())
                .is_none()
        );

        // Natural terrain gives nothing back
        let natural = Vec3::new(3, 3, -2);
        run(&mut world, builder, natural, PlotBuildAction::Remove);
        assert!(was_changed(&world, natural));
        assert_eq!(
            count(&world, builder, "common.items.crafting_ing.stones"),
            0
        );
        assert_eq!(count(&world, builder, WOOD_LOG), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_inventory_keeps_the_block() {
        let dir = test_dir("full");
        let mut world = test_world(&dir);
        // Fill every slot with something other than wood
        let mut inventory = Inventory::with_empty();
        while inventory
            .push(Item::new_from_asset_expect(
                "common.items.armor.misc.foot.sandals",
            ))
            .is_ok()
        {}
        let builder = world.create_entity().with(inventory).build();
        let placed = Vec3::new(5, 5, 5);
        world
            .write_resource::<BuildLedger>()
            .placed(placed, wood(), Payment {
                item: WOOD_LOG.to_owned(),
                amount: 1,
            });

        run(&mut world, builder, placed, PlotBuildAction::Remove);
        assert!(!was_changed(&world, placed));
        assert!(
            world
                .read_resource::<BuildLedger>()
                .refund_for(placed, wood())
                .is_some()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
