//! Player-owned building plots.
//!
//! A plot is one terrain chunk column (32x32 blocks) that a player claims with
//! `/plot claim`. It is an ordinary build area named after its chunk, and the
//! owner, plus anyone they trust, is given permission to build in it. Plot
//! ownership is saved alongside the other build areas by
//! [`crate::build_persistence::BuildPersistence`].

use crate::build_persistence::BuildPersistence;
use authc::Uuid;
use common::{comp::CanBuild, terrain::TerrainChunkSize, vol::RectVolSize};
use common_state::Areas;
use vek::*;

/// The tallest a plot can be, whatever the server settings say.
const MAX_PLOT_HEIGHT: u32 = 1024;

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
}
