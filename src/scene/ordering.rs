use bevy::{
    core_pipeline::core_3d::Transparent3d,
    math::FloatOrd,
    mesh::MeshTag,
    pbr::RenderMeshInstances,
    prelude::*,
    render::{Render, RenderApp, RenderSystems, render_phase::ViewSortedRenderPhases},
};

pub(super) struct StableTransparency;

#[derive(Resource, Default)]
struct DrawSequence(u32);

impl Plugin for StableTransparency {
    fn build(&self, app: &mut App) {
        tag_meshes(app);
        app.sub_app_mut(RenderApp).add_systems(
            Render,
            sort_transparency
                .after(RenderSystems::PhaseSort)
                .before(RenderSystems::Prepare),
        );
    }
}

fn tag_meshes(app: &mut App) {
    app.init_resource::<DrawSequence>().add_observer(
        |added: On<Add<Mesh3d>>,
         mut sequence: ResMut<DrawSequence>,
         mut commands: Commands| {
            sequence.0 = sequence
                .0
                .checked_add(1)
                .expect("render tag space exhausted");
            // Scene meshes are created in model order, independently of entity allocation.
            commands
                .entity(added.entity)
                .insert(MeshTag::new(sequence.0));
        },
    );
}

fn sort_transparency(
    mut phases: ResMut<ViewSortedRenderPhases<Transparent3d>>,
    meshes: Res<RenderMeshInstances>,
) {
    let meshes = meshes.into_inner();
    for phase in phases.0.values_mut() {
        // Bevy's distance-only sort leaves equal-depth draw order unspecified.
        phase.items.sort_by_cached_key(|_, item| {
            let tag = meshes
                .render_mesh_queue_data(item.entity.1)
                .map_or(0, |mesh| mesh.tag());
            (FloatOrd(item.distance), tag)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_order_ignores_entity_allocation_and_survives_replacement() {
        let mut app = App::new();
        tag_meshes(&mut app);
        app.world_mut().spawn_empty();
        let first = app.world_mut().spawn(Mesh3d::default()).id();
        app.world_mut().spawn_empty();
        let second = app.world_mut().spawn(Mesh3d::default()).id();
        app.world_mut().flush();
        assert_eq!(app.world().get::<MeshTag>(first).unwrap().value, 1);
        assert_eq!(app.world().get::<MeshTag>(second).unwrap().value, 2);
        app.world_mut().entity_mut(first).insert(Mesh3d::default());
        app.world_mut().flush();
        assert_eq!(app.world().get::<MeshTag>(first).unwrap().value, 1);
        app.world_mut().despawn(first);
        let third = app.world_mut().spawn(Mesh3d::default()).id();
        app.world_mut().flush();
        assert_eq!(app.world().get::<MeshTag>(third).unwrap().value, 3);
    }
}
