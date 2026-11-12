use std::{fs, path::Path};

use bevy::{
    curve::{Curve, easing::SmoothStepCurve},
    math::Vec3,
    prelude::Transform,
};
use serde::Deserialize;

use crate::{camera::EYE_FLOOR, contributors::Key, model::Simulation};

#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("read camera path {path}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("parse camera path {path}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
    #[error("camera path {path}: {reason}")]
    Invalid { path: String, reason: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    fps: u16,
    key: Vec<Keyframe>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Keyframe {
    frame: u32,
    eye: Anchor,
    focus: Anchor,
    up: Option<[f32; 3]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Anchor {
    point: Option<[f32; 3]>,
    avatar: Option<String>,
    file: Option<String>,
    directory: Option<String>,
    repository: Option<String>,
    #[serde(default)]
    offset: [f32; 3],
    #[serde(skip)]
    target: Option<Target>,
}

enum Target {
    Avatar(Key),
    File(u64),
    Directory(usize),
}

pub(super) struct CameraPath {
    name: String,
    document: Document,
}

impl CameraPath {
    pub(super) fn load(path: &Path, simulation: &Simulation) -> Result<Self, Error> {
        let name = path.display().to_string();
        let text = fs::read_to_string(path).map_err(|source| Error::Read {
            path: name.clone(),
            source,
        })?;
        let mut document: Document =
            toml::from_str(&text).map_err(|source| Error::Parse {
                path: name.clone(),
                source,
            })?;
        let mut validate = || -> Result<(), String> {
            if !(1..=240).contains(&document.fps) || document.key.is_empty() {
                return Err(
                    "fps must be 1..240 and at least one key is required".into()
                );
            }
            let mut previous = None;
            let mut up = Vec3::NEG_Z;
            for key in &mut document.key {
                if previous.is_some_and(|frame| key.frame <= frame) {
                    return Err(format!(
                        "frames must increase strictly at frame {}",
                        key.frame
                    ));
                }
                let next_up = key.up.map_or(up, Vec3::from_array);
                if !next_up.is_finite() || next_up.length_squared() < 0.0001 {
                    return Err(format!(
                        "frame {}: up must be finite and nonzero",
                        key.frame
                    ));
                }
                let next_up = next_up.normalize();
                if previous.is_some() && up.dot(next_up) <= -0.9999 {
                    return Err(format!(
                        "frame {}: consecutive ups must not be opposite",
                        key.frame
                    ));
                }
                up = next_up;
                previous = Some(key.frame);
                key.up = Some(up.to_array());
                key.bind(simulation)?;
            }
            Ok(())
        };
        validate().map_err(|reason| Error::Invalid {
            path: name.clone(),
            reason,
        })?;
        Ok(Self { name, document })
    }

    pub(super) fn pose(
        &self,
        simulation: &Simulation,
        frame: u32,
        fps: u16,
    ) -> Result<Transform, Error> {
        let time = f64::from(frame) * f64::from(self.document.fps) / f64::from(fps);
        let keys = &self.document.key;
        let right = keys.partition_point(|key| f64::from(key.frame) <= time);
        let a = &keys[right.saturating_sub(1)];
        let b = &keys[right.min(keys.len() - 1)];
        #[expect(
            clippy::cast_possible_truncation,
            reason = "normalised interpolation weight is in 0..1"
        )]
        let weight = if a.frame == b.frame {
            0.0
        } else {
            ((time - f64::from(a.frame)) / f64::from(b.frame - a.frame)).clamp(0.0, 1.0)
                as f32
        };
        let weight = SmoothStepCurve.sample_clamped(weight);
        let positions = |key: &Keyframe| -> Result<(Vec3, Vec3), Error> {
            let position = |anchor: &Anchor, channel: &str| {
                anchor.position(simulation).ok_or_else(|| {
                    let target = anchor.avatar.as_deref().or(anchor.file.as_deref()).or(anchor.directory.as_deref()).unwrap_or("point");
                    Error::Invalid { path: self.name.clone(), reason: format!("output frame {frame} key frame {} {channel}: target {target:?} has no position", key.frame) }
                })
            };
            Ok((position(&key.eye, "eye")?, position(&key.focus, "focus")?))
        };
        let (eye, focus) = if weight <= 0.0 {
            positions(a)?
        } else if weight >= 1.0 {
            positions(b)?
        } else {
            let (eye_a, focus_a) = positions(a)?;
            let (eye_b, focus_b) = positions(b)?;
            (eye_a.lerp(eye_b, weight), focus_a.lerp(focus_b, weight))
        };
        let up = Vec3::from_array(a.up.expect("validated up"))
            .lerp(Vec3::from_array(b.up.expect("validated up")), weight)
            .normalize_or_zero();
        let direction = focus - eye;
        let reason = if !eye.is_finite() || !focus.is_finite() {
            Some("resolved positions must be finite")
        } else if eye.y < EYE_FLOOR {
            Some("eye must stay above camera::EYE_FLOOR (1.2 world units)")
        } else if direction.length_squared() < 0.000_000_01 {
            Some("eye and focus must differ by at least 0.0001 world units")
        } else if up.cross(direction.normalize()).length_squared() < 0.000_001 {
            Some("up must not be parallel to view direction")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(Error::Invalid {
                path: self.name.clone(),
                reason: format!("output frame {frame}: {reason}"),
            });
        }
        Ok(Transform::from_translation(eye).looking_at(focus, up))
    }

    pub(super) fn validate_step(
        &self,
        previous: &Transform,
        current: &Transform,
        frame: u32,
    ) -> Result<(), Error> {
        if previous.right().dot(*current.right()) < std::f32::consts::FRAC_1_SQRT_2
            || previous.forward().dot(*current.forward())
                < std::f32::consts::FRAC_1_SQRT_2
        {
            return Err(Error::Invalid {
                path: self.name.clone(),
                reason: format!(
                    "output frames {}..{frame}: camera right or forward reverses or turns more than 45 degrees; add an intermediate key",
                    frame - 1
                ),
            });
        }
        Ok(())
    }
}

impl Keyframe {
    fn bind(&mut self, simulation: &Simulation) -> Result<(), String> {
        for (channel, anchor) in [("eye", &mut self.eye), ("focus", &mut self.focus)] {
            anchor.bind(simulation).map_err(|reason| {
                format!("frame {} {channel}: {reason}", self.frame)
            })?;
        }
        Ok(())
    }
}

impl Anchor {
    fn bind(&mut self, simulation: &Simulation) -> Result<(), String> {
        if !Vec3::from_array(self.offset).is_finite()
            || self
                .point
                .is_some_and(|point| !Vec3::from_array(point).is_finite())
        {
            return Err("coordinates must be finite".into());
        }
        let count = usize::from(self.avatar.is_some())
            + usize::from(self.file.is_some())
            + usize::from(self.directory.is_some())
            + usize::from(self.point.is_some());
        if count != 1 {
            return Err("choose a point, avatar, file or directory".into());
        }
        if self.repository.is_some() && self.file.is_none() && self.directory.is_none()
        {
            return Err("repository requires a file or directory".into());
        }
        if let Some(name) = &self.avatar {
            let mut matches = Vec::new();
            for contributor in simulation
                .camera_events()
                .iter()
                .filter_map(|event| event.contributor.as_ref())
            {
                if &contributor.name == name && !matches.contains(&contributor.key) {
                    matches.push(contributor.key.clone());
                }
            }
            self.target = Some(Target::Avatar(match matches.as_slice() {
                [key] => key.clone(),
                [] => {
                    return Err(format!(
                        "contributor {name:?} not found in selected history"
                    ));
                }
                _ => return Err(format!("contributor {name:?} is ambiguous")),
            }));
        } else if let Some(path) = self.file.as_ref().or(self.directory.as_ref()) {
            let mut matches = Vec::new();
            let layout = simulation.layout();
            for (repository, region) in layout.repositories.iter().enumerate() {
                if self
                    .repository
                    .as_ref()
                    .is_some_and(|label| label != &region.label)
                {
                    continue;
                }
                let internal = if path.is_empty() {
                    repository.to_string()
                } else {
                    format!("{}{path}", crate::repositories::prefix(repository))
                };
                let target = self.path_target(simulation, &internal, &region.label)?;
                if let Some(target) = target {
                    matches.push((region.label.clone(), target));
                }
            }
            match matches.len() {
                1 => self.target = matches.pop().map(|(_, target)| target),
                0 => {
                    let scope = if self.file.is_some() {
                        "selected history"
                    } else {
                        "layout"
                    };
                    return Err(format!("path {path:?} not found in {scope}"));
                }
                _ => {
                    return Err(format!(
                        "path {path:?} is ambiguous; qualify repository: {}",
                        matches
                            .iter()
                            .map(|(label, _)| format!("{label:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
        Ok(())
    }

    fn path_target(
        &self,
        simulation: &Simulation,
        internal: &str,
        repository: &str,
    ) -> Result<Option<Target>, String> {
        if let Some(path) = &self.file {
            match simulation.camera_file_identity(internal) {
                Some(crate::model::FileIdentity::Unique(id)) => {
                    Ok(Some(Target::File(id)))
                }
                Some(crate::model::FileIdentity::Ambiguous) => Err(format!(
                    "file path {path:?} identifies multiple files in repository {repository:?}"
                )),
                None => Ok(None),
            }
        } else {
            let layout = simulation.layout();
            Ok((0..layout.dir_count())
                .find(|&index| layout.dir(index).path == internal)
                .map(Target::Directory))
        }
    }

    fn position(&self, simulation: &Simulation) -> Option<Vec3> {
        let point = match &self.target {
            Some(Target::Avatar(key)) => simulation
                .avatars()
                .iter()
                .find(|avatar| &avatar.key == key)
                .map(|avatar| avatar.position),
            Some(Target::File(id)) => simulation
                .layout()
                .file(*id)
                .map(|file| simulation.file_effect(file).0),
            Some(Target::Directory(index)) => {
                let dir = simulation.layout().dir(*index);
                Some(Vec3::new(
                    dir.position.x,
                    -0.05 + simulation.repository_effect(dir.repository).2,
                    dir.position.y,
                ))
            }
            None => self.point.map(Vec3::from_array),
        };
        point.map(|point| point + Vec3::from_array(self.offset))
    }
}
