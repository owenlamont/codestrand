use bevy::{
    core_pipeline::core_3d::{Transparent3d, TransparentSortingInfo3d},
    math::FloatOrd,
    mesh::MeshTag,
    pbr::{
        MeshInputUniform, MeshUniform, RenderMeshInstances,
        get_mesh_instance_world_from_local,
    },
    prelude::*,
    render::{
        Render, RenderApp, RenderSystems,
        batching::gpu_preprocessing::BatchedInstanceBuffers, mesh::RenderMesh,
        render_asset::RenderAssets, render_phase::ViewSortedRenderPhases,
        view::ExtractedView,
    },
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
    mesh_assets: Res<RenderAssets<RenderMesh>>,
    buffers: Option<Res<BatchedInstanceBuffers<MeshUniform, MeshInputUniform>>>,
    views: Query<&ExtractedView>,
) {
    let meshes = meshes.into_inner();
    let mesh_assets = mesh_assets.into_inner();
    let buffers = buffers.map(Res::into_inner);
    for view in &views {
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else {
            continue;
        };
        let rangefinder = view.rangefinder3d();
        // Bevy retains queue-time centres when only transforms change.
        for item in phase.items.values_mut() {
            if let TransparentSortingInfo3d::Sorted { mesh_center, .. } =
                &mut item.sorting_info
            {
                let Some(mesh) = meshes.render_mesh_queue_data(item.entity.1) else {
                    warn!(entity = ?item.entity.1, "transparent mesh instance missing during sorting");
                    continue;
                };
                let Some(render_mesh) = mesh_assets.get(mesh.mesh_asset_id()) else {
                    warn!(entity = ?item.entity.1, "transparent render mesh missing during sorting");
                    continue;
                };
                if matches!(meshes, RenderMeshInstances::GpuBuilding(_))
                    && buffers.is_none_or(|buffers| {
                        buffers
                            .current_input_buffer
                            .get(mesh.current_uniform_index.0)
                            .is_none()
                    })
                {
                    warn!(entity = ?item.entity.1, "transparent mesh transform missing during sorting");
                    continue;
                }
                *mesh_center = get_mesh_instance_world_from_local(
                    item.entity.1,
                    mesh.current_uniform_index,
                    meshes,
                    buffers,
                )
                .transform_point3(render_mesh.aabb_center);
            }
            item.distance = item.sorting_info.sort_distance(&rangefinder);
        }
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
    use bevy::{
        mesh::{BaseMeshPipelineKey, MeshVertexBufferLayouts},
        pbr::{MeshTransforms, RenderMeshInstanceCpu, RenderMeshInstancesCpu},
        render::{
            material_bind_groups::{
                MaterialBindGroupIndex, MaterialBindGroupSlot, MaterialBindingId,
            },
            mesh::RenderMeshBufferInfo,
            render_phase::{DrawFunctionId, PhaseItemExtraIndex, sort_phase_system},
            render_resource::{CachedRenderPipelineId, TextureFormat},
            sync_world::MainEntity,
            view::RetainedViewEntity,
        },
    };

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

    fn transparent_item(
        entity: Entity,
        mesh_center: Vec3,
        depth_bias: f32,
    ) -> Transparent3d {
        Transparent3d {
            sorting_info: TransparentSortingInfo3d::Sorted {
                mesh_center,
                depth_bias,
            },
            distance: 0.0,
            entity: (Entity::PLACEHOLDER, entity.into()),
            pipeline: CachedRenderPipelineId::INVALID,
            draw_function: DrawFunctionId(0),
            batch_range: 0..1,
            extra_index: PhaseItemExtraIndex::None,
            indexed: false,
        }
    }

    fn moving_transform(z: f32) -> Transform {
        Transform::from_xyz(0.0, 0.0, z)
            .with_rotation(Quat::from_xyzw(0.0, 1.0, 0.0, 0.0))
            .with_scale(Vec3::new(2.0, 1.0, 3.0))
    }

    fn assert_current_sort(
        item: &Transparent3d,
        expected: Vec3,
        bias: f32,
        offset: f32,
    ) {
        let TransparentSortingInfo3d::Sorted {
            mesh_center,
            depth_bias,
        } = item.sorting_info
        else {
            panic!("sorted item")
        };
        assert_eq!(mesh_center, expected);
        assert_eq!(depth_bias, bias);
        assert_eq!(item.distance, expected.z - offset + bias);
    }

    struct MovingPair {
        app: App,
        entities: [Entity; 3],
        views: [RetainedViewEntity; 2],
    }

    impl MovingPair {
        fn new() -> Self {
            let mut app = App::new();
            let entities = std::array::from_fn(|_| app.world_mut().spawn_empty().id());
            let mut assets = Assets::<Mesh>::default();
            let handle = assets.add(Cuboid::default());
            let mut meshes = RenderAssets::<RenderMesh>::default();
            meshes.insert(
                handle.id(),
                RenderMesh {
                    vertex_count: 24,
                    aabb_center: Vec3::new(1.0, 0.0, 1.0),
                    buffer_info: RenderMeshBufferInfo::NonIndexed,
                    key_bits: BaseMeshPipelineKey::empty(),
                    layout: assets.get(&handle).unwrap().get_mesh_vertex_buffer_layout(
                        &mut MeshVertexBufferLayouts::default(),
                    ),
                },
            );
            let mut instances = RenderMeshInstancesCpu::default();
            for (entity, transform, tag) in [
                (entities[0], moving_transform(-5.0), 1),
                (entities[1], Transform::from_xyz(0.0, 0.0, -5.0), 2),
            ] {
                let shared = bevy::pbr::RenderMeshInstanceSharedFlatBlob::default();
                shared.set_mesh_asset_id(handle.id());
                shared.set_tag(tag);
                let affine = transform.compute_affine().into();
                instances.insert(
                    entity.into(),
                    RenderMeshInstanceCpu {
                        shared,
                        transforms: MeshTransforms {
                            world_from_local: affine,
                            previous_world_from_local: affine,
                            flags: 0,
                        },
                        render_layers: None,
                    },
                );
            }
            let views = std::array::from_fn(|_| {
                RetainedViewEntity::new(
                    app.world_mut().spawn_empty().id().into(),
                    None,
                    0,
                )
            });
            let mut phases = ViewSortedRenderPhases::<Transparent3d>::default();
            for (retained_view_entity, z) in [(views[1], 10.0), (views[0], 0.0)] {
                app.world_mut().spawn(ExtractedView {
                    retained_view_entity,
                    clip_from_view: Mat4::IDENTITY,
                    world_from_view: GlobalTransform::from_translation(Vec3::Z * z),
                    clip_from_world: None,
                    target_format: TextureFormat::Rgba8UnormSrgb,
                    viewport: UVec4::new(0, 0, 1280, 720),
                    color_grading: default(),
                    invert_culling: false,
                });
                phases.prepare_for_new_frame(retained_view_entity);
                let phase = phases.get_mut(&retained_view_entity).unwrap();
                phase.add_retained(transparent_item(entities[0], Vec3::ZERO, 0.5));
                phase.add_retained(transparent_item(entities[1], Vec3::ZERO, -0.25));
                let mut top = transparent_item(entities[2], Vec3::ZERO, 0.0);
                top.sorting_info = TransparentSortingInfo3d::AlwaysOnTop;
                top.distance = f32::NEG_INFINITY;
                phase.add_retained(top);
            }
            app.insert_resource(meshes)
                .insert_resource(RenderMeshInstances::CpuBuilding(instances))
                .insert_resource(phases)
                .add_systems(
                    Update,
                    (sort_phase_system::<Transparent3d>, sort_transparency).chain(),
                );
            Self {
                app,
                entities,
                views,
            }
        }

        fn move_first(&mut self, z: f32) {
            let mut meshes = self.app.world_mut().resource_mut::<RenderMeshInstances>();
            let RenderMeshInstances::CpuBuilding(meshes) = &mut *meshes else {
                panic!("CPU fixture")
            };
            meshes
                .get_mut(&MainEntity::from(self.entities[0]))
                .unwrap()
                .transforms
                .world_from_local = moving_transform(z).compute_affine().into();
        }

        fn rebind_first(&mut self) {
            let meshes = self.app.world().resource::<RenderMeshInstances>();
            meshes
                .render_mesh_queue_data(self.entities[0].into())
                .unwrap()
                .shared
                .set_material_bindings_index(MaterialBindingId {
                    group: MaterialBindGroupIndex(1),
                    slot: MaterialBindGroupSlot(7),
                });
            let mut phases = self
                .app
                .world_mut()
                .resource_mut::<ViewSortedRenderPhases<Transparent3d>>();
            for view in self.views {
                phases
                    .get_mut(&view)
                    .unwrap()
                    .add_retained(transparent_item(
                        self.entities[0],
                        Vec3::new(-2.0, 0.0, -2.0),
                        0.5,
                    ));
            }
        }

        fn assert_sorted(&self, order: &[Entity], first_center: Vec3) {
            let phases = self
                .app
                .world()
                .resource::<ViewSortedRenderPhases<Transparent3d>>();
            for (view, offset) in [(self.views[0], 0.0), (self.views[1], 10.0)] {
                let phase = phases.get(&view).unwrap();
                assert_eq!(
                    phase
                        .items
                        .values()
                        .map(|item| item.entity.1)
                        .collect::<Vec<_>>(),
                    order.iter().copied().map(Into::into).collect::<Vec<_>>()
                );
                for (entity, expected, bias) in [
                    (self.entities[0], first_center, 0.5),
                    (self.entities[1], Vec3::new(1.0, 0.0, -4.0), -0.25),
                ] {
                    let item = &phase.items[&(Entity::PLACEHOLDER, entity.into())];
                    assert_current_sort(item, expected, bias, offset);
                }
                let top = &phase.items[&(Entity::PLACEHOLDER, self.entities[2].into())];
                assert!(matches!(
                    top.sorting_info,
                    TransparentSortingInfo3d::AlwaysOnTop
                ));
                assert_eq!(top.distance, f32::NEG_INFINITY);
            }
        }
    }

    #[test]
    fn moving_transparent_pair_refreshes_centres_after_rebind_and_visibility_reentry() {
        let mut pair = MovingPair::new();
        let [first, second, top] = pair.entities;
        pair.app.update();
        pair.assert_sorted(&[top, first, second], Vec3::new(-2.0, 0.0, -8.0));
        pair.move_first(1.0);
        pair.app.update();
        pair.assert_sorted(&[top, second, first], Vec3::new(-2.0, 0.0, -2.0));
        pair.rebind_first();
        pair.move_first(-9.0);
        pair.app.update();
        pair.assert_sorted(&[top, first, second], Vec3::new(-2.0, 0.0, -12.0));
        {
            let mut phases = pair
                .app
                .world_mut()
                .resource_mut::<ViewSortedRenderPhases<Transparent3d>>();
            for view in pair.views {
                phases
                    .get_mut(&view)
                    .unwrap()
                    .remove(Entity::PLACEHOLDER, first.into());
            }
        }
        pair.move_first(1.0);
        pair.app.update();
        {
            let phases = pair
                .app
                .world()
                .resource::<ViewSortedRenderPhases<Transparent3d>>();
            for view in pair.views {
                assert_eq!(phases[&view].items.len(), 2);
            }
        }
        pair.rebind_first();
        pair.move_first(2.0);
        pair.app.update();
        pair.assert_sorted(&[top, second, first], Vec3::new(-2.0, 0.0, -1.0));
        pair.move_first(-1.75);
        pair.app.update();
        pair.assert_sorted(&[top, first, second], Vec3::new(-2.0, 0.0, -4.75));
    }
}
