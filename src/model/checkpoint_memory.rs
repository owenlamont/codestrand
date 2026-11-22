use std::mem::{size_of, size_of_val};

use super::{
    Avatar, Pulse, State,
    names::{self, Visibility},
    slots,
};
use crate::{contributors::Key, diff_samples::Sample};

// Require an explicit accounting decision when a checkpoint type gains a field.
macro_rules! retained_fields {
    ($value:expr, $type:ident { $($heap:ident),+; $($(#[$attr:meta])* $other:ident),* $(,)? }) => {{
        let $type { $($heap,)+ $($(#[$attr])* $other: _,)* } = $value;
        ($($heap,)+)
    }};
}
pub(crate) use retained_fields;

pub(crate) fn buffer_bytes<T>(capacity: usize) -> u128 {
    capacity as u128 * size_of::<T>() as u128
}

pub(crate) fn hash_table_bytes<K, V>(capacity: usize) -> u128 {
    if capacity == 0 {
        return 0;
    }
    // Allow for spare buckets, control bytes and alignment in the standard hash table.
    2 * capacity as u128 * (size_of::<(K, V)>() as u128 + 1) + 32
}

pub(crate) fn tree_bytes<K, V>(len: usize) -> u128 {
    if len == 0 {
        return 0;
    }
    // Rust's tree nodes have eleven key/value slots and at least five occupied slots outside the root.
    let nodes = 1 + (len - 1) / 5;
    nodes as u128 * (11 * size_of::<(K, V)>() + 16 * size_of::<usize>()) as u128
}

impl State {
    pub(super) fn replay_bytes(&self) -> u128 {
        let (layout, avatars, authors, pulses, slot_records, capacities) = retained_fields!(self, State {
            layout, avatars, authors, pulses, slot_records, capacities;
            field_growth, field, region_depth, tick, next_event, focus, distance,
            geometry, avatar_scale, avatar_size, avatar_max_speed, gutter, fragment_pace,
        });
        size_of_val(self) as u128
            + buffer_bytes::<usize>(2)
            + layout.replay_bytes()
            + buffer_bytes::<Avatar>(avatars.capacity())
            + avatars.iter().map(Avatar::replay_bytes).sum::<u128>()
            + hash_table_bytes::<Key, usize>(authors.capacity())
            + authors.keys().map(key_bytes).sum::<u128>()
            + buffer_bytes::<Pulse>(pulses.capacity())
            + pulses.iter().map(Pulse::replay_bytes).sum::<u128>()
            + tree_bytes::<usize, slots::Record>(slot_records.len())
            + buffer_bytes::<u8>(capacities.capacity())
    }
}

impl Avatar {
    fn replay_bytes(&self) -> u128 {
        let (key, name, touched, name_visibility, slot) = retained_fields!(self, Avatar {
            key, name, touched, name_visibility, slot;
            expires, arrived, lifecycle_scale, grey, portrait, position, target, repository,
            last_active, presence, slot_presence, projection_scale, activity, commit,
            slot_origin, group, dismissed, ring_presence, ring_capacity, settle, settled,
            leaving_name, seat, travelling,
        });
        let (placement,) = retained_fields!(name_visibility, Visibility {
            placement; joint_exhausted, joint_clear, opacity, visible, full_clear_since,
        });
        name.capacity() as u128
            + key_bytes(key)
            + buffer_bytes::<u64>(touched.capacity())
            + placement.as_ref().map_or(0, names::Placement::replay_bytes)
            + slot.as_ref().map_or(0, slots::Placement::replay_bytes)
    }
}

impl names::Placement {
    fn replay_bytes(&self) -> u128 {
        let (text,) = retained_fields!(self, Self {
            text; point, body, angle, angular_velocity, commit, scale, motion,
        });
        text.capacity() as u128
    }
}

impl slots::Placement {
    fn replay_bytes(&self) -> u128 {
        let (text,) = retained_fields!(self, Self {
            text; dock, commit, point, offset, half_plane, crossings, overlaps, missing_tile,
        });
        text.capacity() as u128
    }
}

impl Pulse {
    fn replay_bytes(&self) -> u128 {
        let (touched, targets, samples) = retained_fields!(self, Pulse {
            touched, targets, samples; commit, repository, start, end, author, aggregated,
        });
        buffer_bytes::<u64>(touched.capacity())
            + buffer_bytes::<(super::Target, f32, u32, u32)>(targets.capacity())
            + buffer_bytes::<(u64, Sample)>(samples.capacity())
            + samples
                .iter()
                .map(|(_, sample)| {
                    let (text,) = retained_fields!(sample, Sample { text; added });
                    text.capacity() as u128
                })
                .sum::<u128>()
    }
}

pub(super) fn key_bytes(key: &Key) -> u128 {
    match key {
        Key::Email(bytes) | Key::Name(bytes) => bytes.capacity() as u128,
        Key::GitHubLogin(login) => login.capacity() as u128,
        Key::Config(_) | Key::GitHubId(_) => 0,
    }
}
