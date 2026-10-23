//! Saves build areas and build permissions so they survive server restarts.
//!
//! Build areas (`/area_add <name> build ...`) and the permissions players are
//! given for them (`/permit_build`) normally only live in memory. When terrain
//! persistence is enabled, this keeps a copy of them in
//! [`BUILD_AREAS_FILENAME`], restoring areas when the server starts and
//! permissions when a player logs in.

use atomicwrites::{AtomicFile, OverwriteBehavior};
use authc::Uuid;
use common::depot::Id;
use common_state::Areas;
use hashbrown::HashSet;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write as _,
    path::{Path, PathBuf},
};
use tracing::{error, info, warn};
use vek::*;

pub const BUILD_AREAS_FILENAME: &str = "build_areas.ron";

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct SavedArea {
    min: [i32; 3],
    max: [i32; 3],
}

impl From<Aabb<i32>> for SavedArea {
    fn from(aabb: Aabb<i32>) -> Self {
        Self {
            min: aabb.min.into_array(),
            max: aabb.max.into_array(),
        }
    }
}

impl From<SavedArea> for Aabb<i32> {
    fn from(area: SavedArea) -> Self {
        Aabb {
            min: Vec3::from(area.min),
            max: Vec3::from(area.max),
        }
    }
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct SavedBuildData {
    /// Build areas by name. The reserved `world` area is created at startup
    /// and is never stored here.
    areas: BTreeMap<String, SavedArea>,
    /// The names of the build areas each player may build in.
    permits: BTreeMap<Uuid, BTreeSet<String>>,
}

pub struct BuildPersistence {
    path: PathBuf,
    data: SavedBuildData,
    /// False if the existing file couldn't be read, so that it is never
    /// overwritten.
    can_save: bool,
}

impl BuildPersistence {
    /// Load saved build areas and permissions from the data directory.
    ///
    /// A file that cannot be read is moved aside to a backup rather than being
    /// overwritten.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(BUILD_AREAS_FILENAME);
        let data = match std::fs::read_to_string(&path) {
            Ok(contents) => match ron::from_str(&contents) {
                Ok(data) => data,
                Err(error) => {
                    let backup_path = backup_path(&path);
                    error!(
                        ?error,
                        "Failed to parse {:?}, moving it to {:?} and starting with no saved build \
                         areas.",
                        path,
                        backup_path
                    );
                    if let Err(error) = std::fs::rename(&path, &backup_path) {
                        error!(?error, "Failed to back up unreadable build area file");
                    }
                    SavedBuildData::default()
                },
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => SavedBuildData::default(),
            Err(error) => {
                error!(
                    ?error,
                    ?path,
                    "Failed to read build area file, build areas will not be saved"
                );
                return Self {
                    path,
                    data: SavedBuildData::default(),
                    can_save: false,
                };
            },
        };
        info!(
            ?path,
            areas = data.areas.len(),
            players_with_permits = data.permits.len(),
            "Loaded saved build areas"
        );
        Self {
            path,
            data,
            can_save: true,
        }
    }

    /// Add all saved build areas to `areas`.
    pub fn restore_areas(&self, areas: &mut Areas) {
        for (name, area) in &self.data.areas {
            if let Err(name) = areas.insert(name.clone(), (*area).into()) {
                warn!(
                    ?name,
                    "Saved build area conflicts with an existing area, skipping"
                );
            }
        }
    }

    /// The ids of the build areas the player with this uuid may build in.
    pub fn permitted_areas(&self, uuid: Uuid, areas: &Areas) -> HashSet<Id<Aabb<i32>>> {
        self.data
            .permits
            .get(&uuid)
            .into_iter()
            .flatten()
            .filter_map(|name| areas.area_metas().get(name).copied())
            .collect()
    }

    pub fn area_added(&mut self, name: &str, area: Aabb<i32>) {
        self.data.areas.insert(name.to_owned(), area.into());
        self.save();
    }

    /// Forget an area along with every permission to build in it.
    pub fn area_removed(&mut self, name: &str) {
        self.data.areas.remove(name);
        for names in self.data.permits.values_mut() {
            names.remove(name);
        }
        self.data.permits.retain(|_, names| !names.is_empty());
        self.save();
    }

    pub fn permit_added(&mut self, uuid: Uuid, area_name: &str) {
        self.data
            .permits
            .entry(uuid)
            .or_default()
            .insert(area_name.to_owned());
        self.save();
    }

    pub fn permit_removed(&mut self, uuid: Uuid, area_name: &str) {
        if let Some(names) = self.data.permits.get_mut(&uuid) {
            names.remove(area_name);
            if names.is_empty() {
                self.data.permits.remove(&uuid);
            }
        }
        self.save();
    }

    pub fn permits_cleared(&mut self, uuid: Uuid) {
        self.data.permits.remove(&uuid);
        self.save();
    }

    /// Write the current state to disk. These changes come from rare admin
    /// commands, so they are saved straight away.
    fn save(&self) {
        if !self.can_save {
            return;
        }
        let contents = match ron::ser::to_string_pretty(&self.data, Default::default()) {
            Ok(contents) => contents,
            Err(error) => {
                error!(?error, "Failed to serialize build areas");
                return;
            },
        };
        let file = AtomicFile::new(&self.path, OverwriteBehavior::AllowOverwrite);
        if let Err(error) = file.write(|f| f.write_all(contents.as_bytes())) {
            error!(?error, path = ?self.path, "Failed to save build areas");
        }
    }
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

    /// A fresh, empty data directory unique to this test.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "veloren-build-persistence-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn area() -> Aabb<i32> {
        Aabb {
            min: Vec3::new(-10, -20, 0),
            max: Vec3::new(10, 20, 30),
        }
    }

    fn world_areas() -> Areas {
        let mut areas = Areas::default();
        areas
            .insert("world".to_owned(), Aabb {
                min: Vec3::new(0, 0, -100),
                max: Vec3::new(100, 100, 100),
            })
            .unwrap();
        areas
    }

    #[test]
    fn areas_and_permits_survive_restart() {
        let dir = test_dir("restart");
        let uuid = Uuid::from_u128(1);
        {
            let mut persistence = BuildPersistence::load(&dir);
            persistence.area_added("base", area());
            persistence.permit_added(uuid, "base");
            persistence.permit_added(uuid, "world");
        }

        let persistence = BuildPersistence::load(&dir);
        let mut areas = world_areas();
        persistence.restore_areas(&mut areas);
        let base = areas.area_metas()["base"];
        assert_eq!(areas.areas().get(base), Some(&area()));
        assert_eq!(
            persistence.permitted_areas(uuid, &areas),
            [base, areas.area_metas()["world"]].into_iter().collect()
        );
        assert!(
            persistence
                .permitted_areas(Uuid::from_u128(2), &areas)
                .is_empty()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn revoking_and_removing_are_saved() {
        let dir = test_dir("revoke");
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        {
            let mut persistence = BuildPersistence::load(&dir);
            persistence.area_added("base", area());
            persistence.area_added("farm", area());
            persistence.permit_added(a, "base");
            persistence.permit_added(a, "farm");
            persistence.permit_added(b, "farm");
            persistence.permit_added(c, "farm");
            persistence.permit_removed(a, "farm");
            persistence.area_removed("base");
            persistence.permits_cleared(c);
        }

        let persistence = BuildPersistence::load(&dir);
        let mut areas = world_areas();
        persistence.restore_areas(&mut areas);
        assert!(!areas.area_metas().contains_key("base"));
        let farm = areas.area_metas()["farm"];
        assert!(persistence.permitted_areas(a, &areas).is_empty());
        assert_eq!(
            persistence.permitted_areas(b, &areas),
            [farm].into_iter().collect()
        );
        assert!(persistence.permitted_areas(c, &areas).is_empty());
        // Nothing stale is left behind
        assert_eq!(persistence.data.permits.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn conflicting_area_is_skipped() {
        let dir = test_dir("conflict");
        let mut persistence = BuildPersistence::load(&dir);
        persistence.area_added("world", area());
        let mut areas = world_areas();
        persistence.restore_areas(&mut areas);
        assert_ne!(
            areas.areas().get(areas.area_metas()["world"]),
            Some(&area())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_file_is_backed_up_not_overwritten() {
        let dir = test_dir("corrupt");
        let path = dir.join(BUILD_AREAS_FILENAME);
        std::fs::write(&path, "not ron {").unwrap();

        let mut persistence = BuildPersistence::load(&dir);
        assert_eq!(persistence.data, SavedBuildData::default());
        let backup = dir.join("build_areas.ron_backup_0");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "not ron {");

        persistence.area_added("base", area());
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "not ron {");
        assert!(path.is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
