use std::collections::{BTreeMap, HashMap};

use bevy::math::Vec2;
use planner::Plan;

pub(crate) mod outline;
pub(crate) mod packing;
mod planner;
mod repositories;

#[derive(Clone, Debug)]
pub(super) struct RepositoryRegion {
    pub label: String,
    pub low: Vec2,
    pub high: Vec2,
    pub live: bool,
    pub files: f32,
    pub tile: Option<packing::Tile>,
    pub gutter: Option<packing::Tile>,
    pub outline: Vec<Vec2>,
    pub height: f32,
    pub rotated: bool,
}

impl RepositoryRegion {
    pub(super) fn corners(&self) -> [Vec2; 4] {
        [
            self.low,
            self.high,
            Vec2::new(self.low.x, self.high.y),
            Vec2::new(self.high.x, self.low.y),
        ]
    }
}

const SPACING: f32 = 0.62;
const GOLDEN_ANGLE: f32 = 2.399_963_2;
const MAX_SPEED: f32 = 3.0;
const MAX_ACCELERATION: f32 = 48.0;
const FILE_EASE: f32 = 5.0;
const FADE_SECONDS: f32 = 0.6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Presence {
    Arriving,
    Present,
    Leaving,
}

#[derive(Clone, Debug)]
pub(super) struct File {
    local_position: Vec2,
    pub path: String,
    pub dir: usize,
    slot: usize,
    slot_offset: Vec2,
    pub position: Vec2,
    velocity: Vec2,
    pub presence: f32,
    pub state: Presence,
    delay: f32,
    pub heat: f32,
    pub activity: f32,
    pub author: usize,
}

impl File {
    pub(super) fn draw_scale(&self, dir: &Dir) -> f32 {
        match self.state {
            Presence::Present => self.visible_presence(dir),
            Presence::Arriving => dir.scale * (0.15 + 0.85 * self.presence),
            Presence::Leaving => dir.scale,
        }
    }
    fn set_slot(&mut self, slot: usize) {
        self.slot = slot;
        self.slot_offset = spiral(slot);
    }

    pub(super) fn visible_presence(&self, dir: &Dir) -> f32 {
        let clearance = dir.radius() - self.position.distance(dir.position);
        self.presence
            .min((clearance / (0.27 * dir.scale).max(f32::EPSILON)).clamp(0.0, 1.0))
            * dir.scale
    }

    pub(super) fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

#[derive(Clone, Debug)]
pub(super) struct Dir {
    local_position: Vec2,
    pub(super) scale: f32,
    pub(super) repository: usize,
    pub path: String,
    pub parent: Option<usize>,
    pub depth: u16,
    pub position: Vec2,
    slots: Vec<Option<u64>>,
    pub files: usize,
    children: usize,
    pub live: bool,
    pub touch_age: f32,
    reserve: f32,
}

impl Dir {
    fn new(path: String, parent: Option<usize>, depth: u16, repository: usize) -> Self {
        Self {
            local_position: Vec2::ZERO,
            scale: 1.0,
            repository,
            path,
            parent,
            depth,
            position: Vec2::ZERO,
            slots: Vec::new(),
            files: 0,
            children: 0,
            live: true,
            touch_age: f32::INFINITY,
            reserve: 0.4,
        }
    }

    fn core(&self) -> f32 {
        SPACING * slot_count(self.slots.len()).sqrt()
    }

    pub(super) fn radius(&self) -> f32 {
        (self.core() + 0.4) * self.scale
    }

    pub(super) fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// Persistent file and directory positions on the ground plane.
#[derive(Clone, Debug)]
pub(super) struct Layout {
    dirs: Vec<Dir>,
    dir_index: HashMap<String, usize>,
    files: BTreeMap<u64, File>,
    file_index: HashMap<String, u64>,
    next_id: u64,
    revision: u64,
    plan: Plan,
    pub repositories: Vec<RepositoryRegion>,
    placed: bool,
}

impl Layout {
    #[cfg(test)]
    pub(crate) fn planned_time(&self) -> f32 {
        self.plan.time
    }

    fn empty() -> Self {
        Self {
            dirs: vec![Dir::new(String::new(), None, 0, 0)],
            dir_index: HashMap::from([(String::new(), 0)]),
            files: BTreeMap::new(),
            file_index: HashMap::new(),
            next_id: 0,
            revision: 0,
            plan: Plan::default(),
            repositories: Vec::new(),
            placed: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn planned_paths(paths: &[&str], others: usize) -> Self {
        use crate::timeline::{Event, FileChange, Identity, Kind};
        let event = Event {
            seconds: 0,
            author: Identity {
                name: Vec::new(),
                email: Vec::new(),
            },
            repository: 0,
            contributor: None,
            commit: String::new(),
            changes: paths
                .iter()
                .map(|path| (*path).to_owned())
                .chain((0..others).map(|file| format!("other/{file}.rs")))
                .map(|path| FileChange {
                    path,
                    kind: Kind::Added,
                    insertions: 0,
                    removals: 0,
                    samples: Vec::new(),
                })
                .collect(),
        };
        Self::planned(&[event])
    }

    pub(super) fn files(&self) -> impl Iterator<Item = (u64, &File)> {
        self.files.iter().map(|(&id, file)| (id, file))
    }

    pub(super) fn file(&self, id: u64) -> Option<&File> {
        self.files.get(&id)
    }

    pub(super) fn dirs(&self) -> impl Iterator<Item = (usize, &Dir)> {
        self.dirs.iter().enumerate().filter(|(_, dir)| dir.live)
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn dir_count(&self) -> usize {
        self.dirs.len()
    }

    pub(super) fn dir(&self, index: usize) -> &Dir {
        &self.dirs[index]
    }

    pub(super) fn directory_label(&self, index: usize) -> &str {
        let dir = &self.dirs[index];
        dir.path.split_once('/').map_or_else(
            || self.repositories[dir.repository].label.as_str(),
            |(_, path)| path,
        )
    }

    pub(super) fn present_files(&self) -> usize {
        self.files
            .values()
            .filter(|file| file.state != Presence::Leaving)
            .count()
    }

    /// Where a file is heading, which differs from its position while it glides.
    pub(super) fn target(&self, id: u64) -> Option<Vec2> {
        let file = self.files.get(&id)?;
        Some(self.dirs[file.dir].position + spiral(file.slot))
    }

    pub(super) fn add(&mut self, path: &str, delay: f32, author: usize) -> u64 {
        self.revision += 1;
        if let Some(&id) = self.file_index.get(path) {
            let file = self.files.get_mut(&id).expect("indexed files exist");
            file.state = Presence::Arriving;
            file.delay = delay;
            file.author = author;
            file.activity = 1.0;
            self.dirs[file.dir].touch_age = 0.0;
            return id;
        }
        let dir = self.ensure_dir(parent_path(path));
        self.dirs[dir].touch_age = 0.0;
        let slot = self.claim_slot(dir);
        let id = self.next_id;
        self.next_id += 1;
        self.dirs[dir].slots[slot] = Some(id);
        self.dirs[dir].files += 1;
        self.files.insert(
            id,
            File {
                local_position: Vec2::ZERO,
                path: path.to_owned(),
                dir,
                slot,
                slot_offset: spiral(slot),
                position: self.dirs[dir].position + spiral(slot),
                velocity: Vec2::ZERO,
                presence: 0.0,
                state: Presence::Arriving,
                delay,
                heat: 0.0,
                activity: 1.0,
                author,
            },
        );
        self.file_index.insert(path.to_owned(), id);
        id
    }

    pub(super) fn touch(&mut self, path: &str, author: usize) -> u64 {
        match self.file_index.get(path) {
            Some(&id) => {
                let file = self.files.get_mut(&id).expect("indexed files exist");
                file.author = author;
                file.activity = 1.0;
                self.dirs[file.dir].touch_age = 0.0;
                id
            }
            None => self.add(path, 0.0, author),
        }
    }

    pub(super) fn remove(
        &mut self,
        path: &str,
        delay: f32,
        author: usize,
    ) -> Option<u64> {
        let &id = self.file_index.get(path)?;
        let file = self.files.get_mut(&id).expect("indexed files exist");
        file.state = Presence::Leaving;
        file.delay = delay;
        file.author = author;
        file.activity = 1.0;
        self.dirs[file.dir].touch_age = 0.0;
        Some(id)
    }

    /// Keeps the file's identity and current position while it glides to its new slot.
    pub(super) fn rename(&mut self, from: &str, to: &str, author: usize) -> u64 {
        self.revision += 1;
        let Some(id) = self.file_index.remove(from) else {
            return self.add(to, 0.0, author);
        };
        if let Some(previous) = self.file_index.remove(to) {
            self.release(previous);
            self.files.remove(&previous);
        }
        self.dirs[self.files[&id].dir].touch_age = 0.0;
        let dir = self.ensure_dir(parent_path(to));
        let slot = if self.files[&id].dir == dir {
            self.files[&id].slot
        } else {
            let slot = self.claim_slot(dir);
            self.dirs[dir].slots[slot] = Some(id);
            self.dirs[dir].files += 1;
            self.release(id);
            slot
        };
        let file = self.files.get_mut(&id).expect("indexed files exist");
        to.clone_into(&mut file.path);
        file.dir = dir;
        file.set_slot(slot);
        file.author = author;
        file.activity = 1.0;
        self.dirs[file.dir].touch_age = 0.0;
        if file.state == Presence::Leaving {
            file.state = Presence::Arriving;
        }
        self.file_index.insert(to.to_owned(), id);
        id
    }

    pub(super) fn step(&mut self, dt: f32) {
        self.revision += 1;
        self.fade_files(dt);
        self.compact_slots();
        self.prune_empty();
        self.relax(dt);
        for file in self.files.values_mut() {
            let target = self.dirs[file.dir].position + file.slot_offset;
            let desired =
                ((target - file.position) * FILE_EASE).clamp_length_max(MAX_SPEED);
            file.velocity +=
                (desired - file.velocity).clamp_length_max(MAX_ACCELERATION * dt);
            file.position += file.velocity * dt;
        }
    }

    pub(super) fn heat(&mut self, id: u64, amount: f32) {
        let file = self.files.get_mut(&id).expect("touched file exists");
        file.heat = (file.heat + amount).min(1.0);
    }

    pub(super) fn cool_heat(&mut self, dt: f32, half_life: f32) {
        for dir in &mut self.dirs {
            dir.touch_age += dt;
        }
        let decay = (-dt / half_life).exp2();
        for file in self.files.values_mut() {
            file.heat *= decay;
            if file.heat < 0.001 {
                file.heat = 0.0;
            }
            file.activity *= (-dt / 1.6).exp();
        }
    }

    /// Completes pending fades and relaxes for `steps` ticks, for history before playback.
    pub(super) fn settle(&mut self, steps: u32, dt: f32) {
        self.revision += 1;
        for dir in &mut self.dirs {
            dir.touch_age = f32::INFINITY;
        }
        for file in self.files.values_mut() {
            file.delay = 0.0;
            file.heat = 0.0;
            file.activity = 0.0;
            file.presence = match file.state {
                Presence::Leaving => 0.0,
                Presence::Arriving | Presence::Present => 1.0,
            };
        }
        for _ in 0..steps {
            let time = self.plan.time;
            self.step(dt);
            self.plan.time = time;
        }
        for file in self.files.values_mut() {
            file.position = self.dirs[file.dir].position + file.slot_offset;
            file.velocity = Vec2::ZERO;
        }
    }

    fn fade_files(&mut self, dt: f32) {
        let mut gone = Vec::new();
        for (&id, file) in &mut self.files {
            if file.delay > 0.0 {
                file.delay -= dt;
                continue;
            }
            match file.state {
                Presence::Arriving => {
                    file.presence = (file.presence + dt / FADE_SECONDS).min(1.0);
                    if file.presence >= 1.0 {
                        file.state = Presence::Present;
                    }
                }
                Presence::Leaving => {
                    file.presence = (file.presence - dt / 1.2).max(0.0);
                    if file.presence <= 0.0 {
                        gone.push(id);
                    }
                }
                Presence::Present => {}
            }
        }
        for id in gone {
            let file = self.files.remove(&id).expect("faded files exist");
            if self.file_index.get(&file.path) == Some(&id) {
                self.file_index.remove(&file.path);
            }
            self.free_slot(file.dir, file.slot);
        }
    }

    fn release(&mut self, id: u64) {
        let file = &self.files[&id];
        let (dir, slot) = (file.dir, file.slot);
        self.free_slot(dir, slot);
    }

    fn compact_slots(&mut self) {
        for dir in &mut self.dirs {
            if dir.slots.len() <= dir.files.saturating_mul(2) {
                continue;
            }
            let mut packed = Vec::with_capacity(dir.files);
            for &id in dir.slots.iter().flatten() {
                self.files
                    .get_mut(&id)
                    .expect("occupied slots have files")
                    .set_slot(packed.len());
                packed.push(Some(id));
            }
            dir.slots = packed;
        }
    }

    fn free_slot(&mut self, dir: usize, slot: usize) {
        let entry = &mut self.dirs[dir];
        entry.slots[slot] = None;
        entry.files -= 1;
        while entry.slots.last() == Some(&None) {
            entry.slots.pop();
        }
    }

    /// Retires empty directories once per step, never mid-mutation, so a directory
    /// emptied and refilled within one commit keeps its place.
    fn prune_empty(&mut self) {
        for index in 1..self.dirs.len() {
            let dir = &self.dirs[index];
            if dir.live && dir.files == 0 && dir.children == 0 {
                self.prune(index);
            }
        }
    }

    fn prune(&mut self, mut dir: usize) {
        while dir != 0
            && self.dirs[dir].files == 0
            && self.dirs[dir].children == 0
            && self.dirs[dir].reserve < planner::RELEASE_RADIUS
        {
            self.dirs[dir].live = false;
            let Some(parent) = self.dirs[dir].parent else {
                break;
            };
            self.dirs[parent].children -= 1;
            dir = parent;
        }
    }

    fn claim_slot(&mut self, dir: usize) -> usize {
        let slots = &mut self.dirs[dir].slots;
        if let Some(free) = slots.iter().position(Option::is_none) {
            free
        } else {
            slots.push(None);
            slots.len() - 1
        }
    }

    fn ensure_dir(&mut self, path: &str) -> usize {
        if let Some(&index) = self.dir_index.get(path) {
            if !self.dirs[index].live {
                if self.dirs[index].parent.is_some() {
                    let parent = self.ensure_dir(parent_path(path));
                    self.dirs[parent].children += 1;
                }
                let position = self
                    .plan
                    .position(index)
                    .expect("history directories have planned trajectories");
                let dir = &mut self.dirs[index];
                dir.live = true;
                dir.position = position;
            }
            return index;
        }
        let parent = self.ensure_dir(parent_path(path));
        self.dirs[parent].children += 1;
        let index = self.dirs.len();
        self.dirs.push(Dir::new(
            path.to_owned(),
            Some(parent),
            self.dirs[parent].depth + 1,
            self.dirs[parent].repository,
        ));
        self.dir_index.insert(path.to_owned(), index);
        index
    }
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn slot_count(count: usize) -> f32 {
    f32::from(u16::try_from(count).unwrap_or(u16::MAX))
}

fn spiral(slot: usize) -> Vec2 {
    let index = slot_count(slot);
    Vec2::from_angle(index * GOLDEN_ANGLE) * SPACING * (index + 0.5).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    #[test]
    fn slot_offsets_follow_compaction_and_cross_directory_renames() {
        let mut layout = Layout::empty();
        for index in 0..5 {
            layout.add(&format!("source/{index}"), 0.0, 0);
        }
        let kept = layout.file_index["source/4"];
        assert_eq!(layout.files[&kept].slot, 4);
        for index in 0..4 {
            layout.remove(&format!("source/{index}"), 0.0, 0);
        }
        layout.fade_files(1.0);
        layout.compact_slots();
        assert_eq!(layout.files[&kept].slot, 0);
        assert_eq!(layout.files[&kept].slot_offset, spiral(0));
        layout.add("target/occupied", 0.0, 0);
        assert_eq!(layout.rename("source/4", "target/moved", 0), kept);
        assert_eq!(layout.files[&kept].slot, 1);
        assert_eq!(layout.files[&kept].slot_offset, spiral(1));
        assert_eq!(layout.rename("target/moved", "target/renamed", 0), kept);
        assert_eq!(layout.files[&kept].slot_offset, spiral(1));
    }

    #[test]
    fn rename_keeps_identity_and_glides_between_directories() {
        let mut layout = Layout::planned_paths(
            &[
                "src/layout.rs",
                "src/scene/mod.rs",
                "docs/a.md",
                "src/b.md",
                "docs/guide.md",
            ],
            40,
        );
        let id = layout.add("src/layout.rs", 0.0, 0);
        layout.add("src/scene/mod.rs", 0.0, 0);
        layout.settle(300, DT);
        let before = layout.file(id).expect("added file exists").position;
        assert_eq!(layout.rename("src/layout.rs", "src/scene/layout.rs", 0), id);
        let mut previous = before;
        for _ in 0..240 {
            layout.step(DT);
            let position = layout.file(id).expect("renamed file exists").position;
            assert!(position.distance(previous) < 0.5, "file jumped");
            previous = position;
        }
        let file = layout.file(id).expect("renamed file exists");
        assert_eq!(file.path, "src/scene/layout.rs");
        assert!(file.position.distance(before) > 0.5, "file did not move");
    }

    #[test]
    fn renaming_a_sole_file_keeps_its_directory_in_place() {
        let mut layout = Layout::planned_paths(
            &[
                "src/layout.rs",
                "src/scene/mod.rs",
                "docs/a.md",
                "src/b.md",
                "docs/guide.md",
            ],
            40,
        );
        let id = layout.add("docs/a.md", 0.0, 0);
        for file in 0..40 {
            layout.add(&format!("other/{file}.rs"), 0.0, 0);
        }
        layout.settle(600, DT);
        let dir = layout.file(id).expect("added file exists").dir;
        let before = layout.dir(dir).position;
        let radius = layout.dir(dir).radius();
        let target = layout.target(id);
        layout.rename("docs/a.md", "docs/b.md", 0);
        assert_eq!(layout.dir(dir).radius(), radius);
        assert_eq!(layout.target(id), target);
        layout.step(DT);
        assert_eq!(layout.file(id).expect("renamed file exists").dir, dir);
        assert!(
            layout.dir(dir).position.distance(before) < 0.05,
            "directory jumped"
        );
    }

    #[test]
    fn renaming_onto_a_fading_file_keeps_its_directory_in_place() {
        let mut layout = Layout::planned_paths(
            &[
                "src/layout.rs",
                "src/scene/mod.rs",
                "docs/a.md",
                "src/b.md",
                "docs/guide.md",
            ],
            40,
        );
        layout.add("docs/a.md", 0.0, 0);
        let id = layout.add("src/b.md", 0.0, 0);
        for file in 0..40 {
            layout.add(&format!("other/{file}.rs"), 0.0, 0);
        }
        layout.settle(600, DT);
        layout.remove("docs/a.md", 0.6, 0);
        layout.step(DT);
        let docs = layout.dir_index["docs"];
        let before = layout.dir(docs).position;
        layout.rename("src/b.md", "docs/a.md", 0);
        layout.step(DT);
        assert_eq!(layout.file(id).expect("renamed file exists").dir, docs);
        assert!(
            layout.dir(docs).position.distance(before) < 0.05,
            "directory jumped"
        );
    }

    #[test]
    fn deletion_fades_then_prunes_empty_directories() {
        let mut layout = Layout::planned_paths(
            &[
                "src/layout.rs",
                "src/scene/mod.rs",
                "docs/a.md",
                "src/b.md",
                "docs/guide.md",
            ],
            40,
        );
        let id = layout.add("docs/guide.md", 0.0, 0);
        layout.settle(10, DT);
        layout.remove("docs/guide.md", 0.5, 0);
        layout.step(DT);
        assert_eq!(layout.file(id).map(|file| file.presence), Some(1.0));
        for _ in 0..180 {
            layout.step(DT);
        }
        assert!(layout.file(id).is_none());
        assert_eq!(layout.dirs().count(), 1);
    }

    #[test]
    fn sibling_directories_separate() {
        let paths: Vec<_> = ["a", "b", "c", "d"]
            .into_iter()
            .flat_map(|dir| (0..12).map(move |file| format!("{dir}/{file}.rs")))
            .collect();
        let mut layout = Layout::planned_paths(
            &paths.iter().map(String::as_str).collect::<Vec<_>>(),
            0,
        );
        for name in ["a", "b", "c", "d"] {
            for file in 0..12 {
                layout.add(&format!("{name}/{file}.rs"), 0.0, 0);
            }
        }
        layout.settle(900, DT);
        let dirs: Vec<_> = layout.dirs().skip(1).map(|(_, dir)| dir).collect();
        for (index, a) in dirs.iter().enumerate() {
            for b in &dirs[index + 1..] {
                let clearance =
                    a.position.distance(b.position) - a.radius() - b.radius();
                assert!(clearance > 0.0, "{} overlaps {}", a.path, b.path);
            }
        }
    }
}
