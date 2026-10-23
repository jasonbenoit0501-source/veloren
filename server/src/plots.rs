//! Player-owned building plots.
//!
//! A plot is one terrain chunk column (32x32 blocks) that a player claims with
//! `/plot claim`. It is an ordinary build area named after its chunk, and the
//! owner, plus anyone they trust, is given permission to build in it. Plot
//! ownership is saved alongside the other build areas by
//! [`crate::build_persistence::BuildPersistence`].
//!
//! Plots are also protected from mining and explosions caused by anyone who
//! may not build in them (see [`is_protected`]).

use crate::build_persistence::BuildPersistence;
use authc::Uuid;
use common::{
    comp::CanBuild,
    terrain::{TerrainChunkSize, TerrainGrid},
    vol::RectVolSize,
};
use common_state::Areas;
use vek::*;

/// The tallest a plot can be, whatever the server settings say.
const MAX_PLOT_HEIGHT: u32 = 1024;

/// How far, in blocks, plots must stay from the edge of a settlement or
/// dungeon.
pub const SITE_MARGIN: f32 = 16.0;

/// The name of the build area for the plot in this chunk.
pub fn plot_name(chunk_key: Vec2<i32>) -> String { format!("plot_{}_{}", chunk_key.x, chunk_key.y) }

/// The area covered by a plot in this chunk, claimed from height `claim_z`.
///
/// The bounds are inclusive, so the plot covers exactly one chunk and never
/// touches its neighbours.
pub fn plot_area(chunk_key: Vec2<i32>, claim_z: i32, height: u32) -> Aabb<i32> {
    let size = TerrainChunkSize::RECT_SIZE.as_::<i32>();
    let min = chunk_key * size;
    let height = height.clamp(1, MAX_PLOT_HEIGHT) as i32;
    let bottom = claim_z - height / 4;
    Aabb {
        min: Vec3::new(min.x, min.y, bottom),
        max: Vec3::new(min.x + size.x - 1, min.y + size.y - 1, bottom + height - 1),
    }
}

/// Whether a player with these build permissions may place or remove the
/// block at `pos`. This is what keeps players out of each other's plots.
pub fn may_build_at(can_build: &CanBuild, areas: &Areas, pos: Vec3<i32>) -> bool {
    can_build.build_areas.iter().any(|area| {
        areas
            .areas()
            .get(*area)
            // TODO: Make this an exclusive check on the upper bound of the AABB
            // Vek defaults to inclusive which is not optimal
            .is_some_and(|aabb| aabb.contains_point(pos))
    })
}

/// Whether the block at `pos` is inside a claimed plot that `builder` may not
/// change. `builder` is `None` for changes not caused by a player, such as a
/// creature's explosion.
///
/// This is always false outside plots, so the rest of the world is unaffected.
pub fn is_protected(
    pos: Vec3<i32>,
    builder: Option<&CanBuild>,
    areas: &Areas,
    build_persistence: &BuildPersistence,
) -> bool {
    if !build_persistence.has_plots() {
        return false;
    }
    let name = plot_name(TerrainGrid::chunk_key(pos));
    build_persistence.plot_owner(&name).is_some()
        && areas
            .area_metas()
            .get(&name)
            .and_then(|id| areas.areas().get(*id))
            .is_some_and(|aabb| aabb.contains_point(pos))
        && !builder.is_some_and(|builder| may_build_at(builder, areas, pos))
}

/// Whether a plot covering `area` would come within [`SITE_MARGIN`] of any of
/// these sites (settlements, dungeons, ...), given as their centre and radius.
pub fn overlaps_site(area: Aabb<i32>, sites: impl IntoIterator<Item = (Vec2<i32>, f32)>) -> bool {
    sites.into_iter().any(|(origin, radius)| {
        let closest = origin.clamped(area.min.xy(), area.max.xy());
        closest.as_::<f32>().distance(origin.as_()) <= radius + SITE_MARGIN
    })
}

#[derive(Debug, PartialEq)]
pub enum ClaimError {
    /// The player already owns the maximum number of plots.
    TooManyPlots { max: u32 },
    /// The plot would overlap the named build area.
    Overlaps(String),
}

/// Check whether `owner` may claim a new plot covering `area`.
///
/// The reserved `world` build area covers everything and is ignored.
pub fn check_claim(
    owner: Uuid,
    area: Aabb<i32>,
    areas: &Areas,
    build_persistence: &BuildPersistence,
    max_plots: u32,
) -> Result<(), ClaimError> {
    if build_persistence.plots_owned_by(owner).count() >= max_plots as usize {
        return Err(ClaimError::TooManyPlots { max: max_plots });
    }
    for (name, id) in areas.area_metas() {
        if name != "world"
            && areas
                .areas()
                .get(*id)
                .is_some_and(|other| other.collides_with_aabb(area))
        {
            return Err(ClaimError::Overlaps(name.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("veloren-plots-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn world_areas() -> Areas {
        let mut areas = Areas::default();
        areas
            .insert("world".to_owned(), Aabb {
                min: Vec3::new(0, 0, -1000),
                max: Vec3::new(10000, 10000, 1000),
            })
            .unwrap();
        areas
    }

    #[test]
    fn plot_covers_exactly_one_chunk() {
        let area = plot_area(Vec2::new(2, -1), 100, 128);
        assert_eq!(area.min, Vec3::new(64, -32, 68));
        assert_eq!(area.max, Vec3::new(95, -1, 195));
        assert!(area.contains_point(Vec3::new(80, -16, 100)));
        // Neighbouring plots don't overlap
        assert!(!area.collides_with_aabb(plot_area(Vec2::new(3, -1), 100, 128)));
        assert!(!area.collides_with_aabb(plot_area(Vec2::new(2, 0), 100, 128)));
        assert_eq!(plot_name(Vec2::new(2, -1)), "plot_2_-1");
    }

    #[test]
    fn claim_limits() {
        let dir = test_dir("limits");
        let mut persistence = BuildPersistence::load(&dir);
        let mut areas = world_areas();
        let (alice, bob) = (Uuid::from_u128(1), Uuid::from_u128(2));

        let first = plot_area(Vec2::new(1, 1), 50, 128);
        assert_eq!(check_claim(alice, first, &areas, &persistence, 1), Ok(()));
        areas.insert(plot_name(Vec2::new(1, 1)), first).unwrap();
        persistence.plot_claimed(&plot_name(Vec2::new(1, 1)), first, alice);

        // Over the per-player limit
        let second = plot_area(Vec2::new(5, 5), 50, 128);
        assert_eq!(
            check_claim(alice, second, &areas, &persistence, 1),
            Err(ClaimError::TooManyPlots { max: 1 })
        );
        // Someone else's plot can't be claimed over
        assert_eq!(
            check_claim(bob, first, &areas, &persistence, 1),
            Err(ClaimError::Overlaps("plot_1_1".to_owned()))
        );
        // But a free chunk can
        assert_eq!(check_claim(bob, second, &areas, &persistence, 1), Ok(()));
        // Disabled entirely
        assert_eq!(
            check_claim(bob, second, &areas, &persistence, 0),
            Err(ClaimError::TooManyPlots { max: 0 })
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_owner_and_trusted_may_build_after_restart() {
        let dir = test_dir("permissions");
        let (owner, friend, stranger) =
            (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let key = Vec2::new(3, 4);
        let area = plot_area(key, 20, 128);
        {
            let mut persistence = BuildPersistence::load(&dir);
            persistence.plot_claimed(&plot_name(key), area, owner);
            persistence.permit_added(friend, &plot_name(key));
            persistence.permit_added(stranger, &plot_name(key));
            persistence.permit_removed(stranger, &plot_name(key));
        }

        // Simulate a server restart followed by each player logging in
        let persistence = BuildPersistence::load(&dir);
        let mut areas = world_areas();
        persistence.restore_areas(&mut areas);
        let can_build = |uuid| CanBuild {
            enabled: true,
            build_areas: persistence.permitted_areas(uuid, &areas),
        };
        let inside = Vec3::new(100, 140, 30);
        let outside = Vec3::new(132, 140, 30);
        assert!(may_build_at(&can_build(owner), &areas, inside));
        assert!(may_build_at(&can_build(friend), &areas, inside));
        assert!(!may_build_at(&can_build(stranger), &areas, inside));
        // Owning a plot doesn't allow building next to it
        assert!(!may_build_at(&can_build(owner), &areas, outside));
        assert_eq!(persistence.plot_owner(&plot_name(key)), Some(owner));
        assert_eq!(
            persistence
                .trusted_players(&plot_name(key))
                .collect::<Vec<_>>(),
            vec![friend]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unclaiming_removes_all_permissions() {
        let dir = test_dir("unclaim");
        let (owner, friend) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let name = plot_name(Vec2::new(0, 0));
        {
            let mut persistence = BuildPersistence::load(&dir);
            persistence.plot_claimed(&name, plot_area(Vec2::new(0, 0), 0, 64), owner);
            persistence.permit_added(friend, &name);
            persistence.area_removed(&name);
        }

        let persistence = BuildPersistence::load(&dir);
        let mut areas = world_areas();
        persistence.restore_areas(&mut areas);
        assert!(!areas.area_metas().contains_key(&name));
        assert_eq!(persistence.plot_owner(&name), None);
        assert_eq!(persistence.plots_owned_by(owner).count(), 0);
        assert!(persistence.permitted_areas(owner, &areas).is_empty());
        assert!(persistence.permitted_areas(friend, &areas).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mining_and_explosions_respect_plots() {
        let dir = test_dir("protection");
        let (owner, friend, stranger) =
            (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let key = Vec2::new(2, 2);
        let name = plot_name(key);
        let area = plot_area(key, 40, 128);
        let mut persistence = BuildPersistence::load(&dir);
        let mut areas = world_areas();
        let inside = Vec3::new(70, 70, 50);

        // No plots claimed yet: nothing is protected
        assert!(!is_protected(inside, None, &areas, &persistence));

        areas.insert(name.clone(), area).unwrap();
        persistence.plot_claimed(&name, area, owner);
        persistence.permit_added(friend, &name);
        let builder = |uuid| CanBuild {
            enabled: false,
            build_areas: persistence.permitted_areas(uuid, &areas),
        };
        let admin = CanBuild {
            enabled: false,
            build_areas: [areas.area_metas()["world"]].into_iter().collect(),
        };

        // Inside the plot
        assert!(!is_protected(
            inside,
            Some(&builder(owner)),
            &areas,
            &persistence
        ));
        assert!(!is_protected(
            inside,
            Some(&builder(friend)),
            &areas,
            &persistence
        ));
        assert!(!is_protected(inside, Some(&admin), &areas, &persistence));
        assert!(is_protected(
            inside,
            Some(&builder(stranger)),
            &areas,
            &persistence
        ));
        // e.g. a creature's explosion, or a player with no build permissions
        assert!(is_protected(inside, None, &areas, &persistence));

        // Outside the plot, sideways or above it, nothing changes
        assert!(!is_protected(
            Vec3::new(96, 70, 50),
            None,
            &areas,
            &persistence
        ));
        assert!(!is_protected(
            Vec3::new(70, 70, 500),
            None,
            &areas,
            &persistence
        ));

        // An admin-made build area that isn't a plot isn't protected
        areas
            .insert("plot_9_9".to_owned(), plot_area(Vec2::new(9, 9), 0, 64))
            .unwrap();
        assert!(!is_protected(
            Vec3::new(300, 300, 10),
            None,
            &areas,
            &persistence
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plots_keep_away_from_sites() {
        // Plot covering x and y from 64 to 95
        let area = plot_area(Vec2::new(2, 2), 0, 64);
        // A settlement centred inside the plot
        assert!(overlaps_site(area, [(Vec2::new(80, 80), 1.0)]));
        // A big settlement whose edge reaches the plot
        assert!(overlaps_site(area, [(Vec2::new(200, 80), 120.0)]));
        // Close enough to fall within the margin
        assert!(overlaps_site(area, [(Vec2::new(200, 80), 100.0)]));
        // Far enough away
        assert!(!overlaps_site(area, [(Vec2::new(200, 80), 50.0)]));
        assert!(!overlaps_site(area, []));
    }
}
