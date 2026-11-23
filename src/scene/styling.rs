use std::hash::BuildHasher as _;

use crate::{model::Simulation, theme::Theme};
use bevy::{
    platform::hash::FixedHasher,
    prelude::*,
    text::{
        ComputedTextBlock, FontAtlasKey, FontAtlasSet, GlyphCacheKey, ScaleCx,
        add_glyph_to_atlas, get_outlined_glyph_texture,
    },
    ui::{ComputedNode, widget::TextNodeFlags},
};
use swash::FontRef;

use super::{
    Label, Palette, SceneCamera, grow, hide_rest, place_beam, set_material, set_visible,
};
use crate::theme::{AvatarStyle, BeamStyle, FontStyle};

pub(super) fn label_font(primary: Handle<Font>, fallback: Handle<Font>) -> FontSource {
    FontSource::list([primary.into(), fallback.into(), FontSource::sans_serif()])
}

const MAX_FONT_ATLAS_PAGES: usize = 64;

#[derive(Default, Resource)]
pub(super) struct FontAtlasBudget {
    pub(super) exceeded: bool,
}

type AtlasLayout = (
    Ref<'static, ComputedNode>,
    &'static TextLayout,
    Ref<'static, ComputedTextBlock>,
    Ref<'static, FontHinting>,
    &'static mut TextNodeFlags,
);

#[expect(
    clippy::too_many_arguments,
    reason = "Bevy injects the text and asset resources"
)]
pub(super) fn reserve_font_atlases(
    mut atlases: ResMut<FontAtlasSet>,
    mut textures: ResMut<Assets<Image>>,
    mut scale: ResMut<ScaleCx>,
    mut layouts: Query<AtlasLayout>,
    fonts: Res<Assets<Font>>,
    positioned: Query<&bevy::text::TextLayoutInfo>,
    mut budget: ResMut<FontAtlasBudget>,
    mut exits: MessageWriter<AppExit>,
) {
    if !budget.exceeded {
        let mut pending = layouts
            .iter_mut()
            .filter(|(node, _, computed, hinting, flags)| {
                node.is_changed()
                    || computed.is_changed()
                    || hinting.is_changed()
                    || flags.is_changed()
            })
            .peekable();
        if pending.peek().is_some() {
            drop_stale_font_atlases(
                &mut atlases,
                &mut textures,
                fonts.into_inner(),
                positioned.iter(),
            );
        }
        let mut reservation = AtlasReservation {
            pages: atlases.values().map(Vec::len).sum(),
            atlases: &mut atlases,
            textures: &mut textures,
            scale: &mut scale,
        };
        let fits = pending.all(|(node, text, computed, hinting, _)| {
            reservation.layout(&node, *text, &computed, *hinting)
        });
        if !fits {
            budget.exceeded = true;
            bevy::log::error!("font atlas exceeded {MAX_FONT_ATLAS_PAGES} pages");
            exits.write(AppExit::error());
        }
    }
    if budget.exceeded {
        // Bevy skips text layout while the flags indicate that measurement is pending.
        for (_, _, _, _, mut flags) in &mut layouts {
            *flags = TextNodeFlags::default();
        }
    }
}

struct AtlasReservation<'a> {
    pages: usize,
    atlases: &'a mut FontAtlasSet,
    textures: &'a mut Assets<Image>,
    scale: &'a mut ScaleCx,
}

impl AtlasReservation<'_> {
    fn layout(
        &mut self,
        node: &ComputedNode,
        text: TextLayout,
        computed: &ComputedTextBlock,
        hinting: FontHinting,
    ) -> bool {
        let mut buffer;
        let layout = if text.linebreak == bevy::text::LineBreak::NoWrap {
            computed.buffer()
        } else {
            buffer = computed.buffer().clone();
            buffer.break_all_lines(Some(node.content_box().width()));
            &buffer
        };
        for run in layout.lines().flat_map(|line| line.runs()) {
            let font = run.font();
            let coords = run.normalized_coords();
            let variations_hash = FixedHasher.hash_one(coords);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "Bevy truncates the font data ID in its atlas key"
            )]
            let id = font.data.id() as u32;
            for glyph in run.visual_clusters().flat_map(|cluster| cluster.glyphs()) {
                let Ok(glyph_id) = u16::try_from(glyph.id) else {
                    continue;
                };
                let font_smoothing =
                    layout.styles()[glyph.style_index()].brush.font_smoothing;
                let key = FontAtlasKey {
                    id,
                    index: font.index,
                    font_size_bits: run.font_size().to_bits(),
                    variations_hash,
                    hinting,
                    font_smoothing,
                };
                if !self.glyph(
                    key,
                    glyph_id,
                    font.data.as_ref(),
                    font.data.id(),
                    coords,
                ) {
                    return false;
                }
            }
        }
        true
    }

    fn glyph(
        &mut self,
        font: FontAtlasKey,
        glyph_id: u16,
        data: &[u8],
        data_id: u64,
        coords: &[i16],
    ) -> bool {
        let glyph = GlyphCacheKey { glyph_id };
        if self.atlases.has_glyph(glyph, &font) {
            return true;
        }
        let Some(font_ref) = FontRef::from_index(data, font.index as usize) else {
            return true;
        };
        let smoothing = font.font_smoothing;
        let mut scaler = self
            .scale
            .0
            .builder_with_id(font_ref, [data_id, font.index.into()])
            .size(f32::from_bits(font.font_size_bits))
            .hint(font.hinting.is_enabled() && smoothing == FontSmoothing::AntiAliased)
            .normalized_coords(coords)
            .build();
        if self.pages < MAX_FONT_ATLAS_PAGES {
            let atlases = self.atlases.entry(font).or_default();
            let before = atlases.len();
            let result = add_glyph_to_atlas(
                atlases,
                self.textures,
                &mut scaler,
                smoothing,
                glyph_id,
            );
            self.pages += atlases.len() - before;
            return result.is_ok()
                || matches!(
                    result,
                    Err(bevy::text::TextError::FailedToGetGlyphImage(_))
                );
        }
        let Ok((texture, offset, alpha)) =
            get_outlined_glyph_texture(&mut scaler, glyph_id, smoothing)
        else {
            return true;
        };
        self.atlases.get_mut(&font).is_some_and(|atlases| {
            atlases.iter_mut().any(|atlas| {
                atlas
                    .add_glyph(self.textures, glyph, &texture, offset, alpha)
                    .is_ok()
            })
        })
    }
}

/// Drops font atlases that no laid-out text uses, unless their font is a loaded asset.
///
/// Bevy evicts a system fallback font after two unused frames and reloads it under a
/// new id, so the atlases of earlier ids would otherwise accumulate for the whole run.
fn drop_stale_font_atlases<'a>(
    atlases: &mut FontAtlasSet,
    textures: &mut Assets<Image>,
    fonts: &Assets<Font>,
    layouts: impl Iterator<Item = &'a bevy::text::TextLayoutInfo>,
) {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "bevy_text keys atlases by the truncated font data id"
    )]
    let loaded: std::collections::HashSet<u32> = fonts
        .iter()
        .map(|(_, font)| font.data.id() as u32)
        .collect();
    if atlases.keys().all(|key| loaded.contains(&key.id)) {
        return;
    }
    let used: std::collections::HashSet<AssetId<Image>> = layouts
        .flat_map(|layout| &layout.glyphs)
        .map(|glyph| glyph.atlas_info.texture)
        .collect();
    atlases.retain(|key, font_atlases| {
        if !loaded.contains(&key.id) {
            for atlas in
                font_atlases.extract_if(.., |atlas| !used.contains(&atlas.texture.id()))
            {
                textures.remove(atlas.texture.id());
            }
        }
        !font_atlases.is_empty()
    });
}

#[derive(Default)]
struct Territories {
    revision: Option<u64>,
    owners: Vec<usize>,
    radii: Vec<f32>,
}

impl Territories {
    fn update(&mut self, layout: &crate::layout::Layout) {
        if self.revision == Some(layout.revision()) {
            return;
        }
        let first = self.owners.len();
        self.owners.resize(layout.dir_count(), 0);
        self.radii.clear();
        self.radii.resize(layout.dir_count(), 1.0);
        for index in first..layout.dir_count() {
            self.owners[index] = layout.dir(index).parent.map_or(index, |parent| {
                if layout.dir(parent).parent.is_none() {
                    index
                } else {
                    self.owners[parent]
                }
            });
        }
        for (_, file) in layout.files() {
            let owner = self.owners[file.dir];
            if layout.dir(owner).parent.is_some() {
                self.radii[owner] = self.radii[owner]
                    .max(file.position.distance(layout.dir(owner).position));
            }
        }
        self.revision = Some(layout.revision());
    }
}

pub(super) fn edge_segments(theme: &Theme, from: Vec3, to: Vec3) -> Vec<(Vec3, Vec3)> {
    let count = if theme.edge_dash.is_some() { 12_u16 } else { 1 };
    (0..count)
        .map(|i| {
            let start = f32::from(i) / f32::from(count);
            let end =
                (f32::from(i) + theme.edge_dash.unwrap_or(1.0)) / f32::from(count);
            (from.lerp(to, start), from.lerp(to, end))
        })
        .collect()
}
pub(super) const BEAM_SEGMENTS: usize = 16;

#[derive(Resource)]
pub(super) struct Styles {
    territory_mesh: Handle<Mesh>,
    fonts: Vec<(FontStyle, Handle<Font>)>,
    pub(super) beams: Vec<Handle<StandardMaterial>>,
    pub(super) territories: Vec<Handle<StandardMaterial>>,
    territory_pool: Vec<Entity>,
    territory_layout: Territories,
    leaders: Vec<Entity>,
}

pub(super) fn reset_pools(world: &mut World) {
    if world.contains_resource::<Styles>() {
        world.resource_scope(|world, mut styles: Mut<Styles>| {
            for entity in std::mem::take(&mut styles.territory_pool)
                .into_iter()
                .chain(std::mem::take(&mut styles.leaders))
            {
                world.despawn(entity);
            }
            styles.territory_layout = Territories::default();
        });
    }
}

#[cfg(test)]
pub(super) fn seed_replay_pools(world: &mut World) -> [Entity; 2] {
    let territory = world.spawn_empty().id();
    let leader = world.spawn_empty().id();
    let mut styles = world.resource_mut::<Styles>();
    styles.territory_pool.push(territory);
    styles.leaders.push(leader);
    [territory, leader]
}

pub(super) fn setup(world: &mut World, theme: &Theme) {
    let territory_mesh = world.resource_mut::<Assets<Mesh>>().add(Annulus::new(
        theme.avatar_radius * 0.99,
        theme.avatar_radius,
    ));
    let fonts = [
        (
            FontStyle::Tech,
            include_bytes!("../../assets/fonts/ShareTechMono-Regular.ttf").as_slice(),
        ),
        (
            FontStyle::Plex,
            include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf").as_slice(),
        ),
        (
            FontStyle::Serif,
            include_bytes!("../../assets/fonts/Fraunces[SOFT,WONK,opsz,wght].ttf")
                .as_slice(),
        ),
    ]
    .into_iter()
    .map(|(kind, bytes)| {
        (
            kind,
            world
                .resource_mut::<Assets<Font>>()
                .add(Font::from_bytes(bytes.to_vec())),
        )
    })
    .collect();
    let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
    let beams = (0..BEAM_SEGMENTS)
        .map(|_| materials.add(theme.edge.clone()))
        .collect();
    let territories = (0..36).map(|_| materials.add(theme.edge.clone())).collect();
    world.insert_resource(Styles {
        territory_mesh,
        fonts,
        beams,
        territories,
        territory_pool: Vec::new(),
        territory_layout: Territories::default(),
        leaders: Vec::new(),
    });
    apply(world, theme);
}

pub(super) fn avatar_mesh(theme: &Theme) -> Mesh {
    let r = theme.avatar_radius;
    match theme.avatar_style {
        AvatarStyle::Sphere => Sphere::new(r).into(),
        AvatarStyle::Reticle => {
            let mut mesh = Mesh::from(Annulus::new(r * 0.7, r * 0.8));
            for (x, y, sx, sy) in [
                (0.0, r, r * 0.1, r * 0.7),
                (0.0, -r, r * 0.1, r * 0.7),
                (r, 0.0, r * 0.7, r * 0.1),
                (-r, 0.0, r * 0.7, r * 0.1),
            ] {
                mesh.merge(
                    &Mesh::from(Rectangle::new(sx, sy))
                        .translated_by(Vec3::new(x, y, 0.0)),
                )
                .expect("reticle attributes");
            }
            mesh
        }
        AvatarStyle::Pointer => Triangle2d::new(
            Vec2::new(-r, r),
            Vec2::new(-r * 0.5, -r),
            Vec2::new(r, -r * 0.25),
        )
        .into(),
        AvatarStyle::Pin => {
            let mut mesh =
                Mesh::from(Circle::new(r * 0.65)).translated_by(Vec3::Y * r * 0.35);
            mesh.merge(&Mesh::from(Triangle2d::new(
                Vec2::new(-r * 0.5, 0.0),
                Vec2::new(0.0, -r),
                Vec2::new(r * 0.5, 0.0),
            )))
            .expect("pin attributes");
            mesh
        }
    }
}

pub(super) fn ring_mesh(theme: &Theme, radii: Vec2) -> Mesh {
    if theme.round_rings() {
        return Annulus::new(radii.x, radii.y).into();
    }
    let width = radii.y - radii.x;
    let centre = radii.x + width * 0.5;
    let mut mesh = Mesh::from(Rectangle::new(radii.y * 2.0, width))
        .translated_by(Vec3::Y * centre);
    for (x, y, sx, sy) in [
        (0.0, -centre, radii.y * 2.0, width),
        (-centre, 0.0, width, radii.x * 2.0),
        (centre, 0.0, width, radii.x * 2.0),
    ] {
        mesh.merge(
            &Mesh::from(Rectangle::new(sx, sy)).translated_by(Vec3::new(x, y, 0.0)),
        )
        .expect("outline attributes");
    }
    mesh
}

pub(super) fn apply(world: &mut World, theme: &Theme) {
    let cameras: Vec<_> = world
        .query_filtered::<Entity, With<SceneCamera>>()
        .iter(world)
        .collect();
    for entity in cameras {
        if let Some(bloom) = theme.bloom() {
            world.entity_mut(entity).insert(bloom);
        } else {
            world
                .entity_mut(entity)
                .remove::<bevy::post_process::bloom::Bloom>();
        }
    }
    world.resource_scope(|world, styles: Mut<Styles>| {
        let font = styles.fonts.iter().find(|(kind, _)| *kind == theme.font).map(|(_, handle)| handle.clone()).unwrap_or_default();
        let code_style = match theme.font {
            FontStyle::Serif => FontStyle::Plex,
            monospace => monospace,
        };
        let code_font = styles.fonts.iter()
            .find(|(kind, _)| *kind == code_style)
            .map(|(_, handle)| handle.clone())
            .unwrap_or_default();
        let fallback = styles.fonts.iter().find(|(kind, _)| *kind == FontStyle::Plex)
            .expect("embedded Plex fallback").1.clone();
        for (mut text, snippet, caption) in world
            .query::<(&mut TextFont, Option<&super::DiffSample>, Option<&super::caption::CaptionText>)>().iter_mut(world)
        {
            let primary = if caption.is_some() {
                Handle::default()
            } else if snippet.is_some() {
                code_font.clone()
            } else {
                font.clone()
            };
            text.font = label_font(primary, fallback.clone());
        }
        for (mut node, mut background, mut border) in world.query_filtered::<(&mut Node, &mut BackgroundColor, &mut BorderColor), With<Label>>().iter_mut(world) {
            node.border = UiRect::all(px(if theme.label_style == crate::theme::LabelStyle::Bevelled { 2.0 } else { 0.0 }));
            background.0 = theme.label_background();
            *border = theme.label_border();
        }
        let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
        for (index, handle) in styles.beams.iter().enumerate() {
            let mut material = materials.get_mut(handle).expect("beam style");
            *material = theme.beam_material(u16::try_from(index).expect("16 segments"));
        }
        for (index, handle) in styles.territories.iter().enumerate() {
            let mut material = materials.get_mut(handle).expect("territory");
            *material = theme.territory_material(index / 3, index % 3 != 0);
        }
    });
}

pub(super) fn beam_segments(
    theme: &Theme,
    from: Vec3,
    to: Vec3,
) -> Vec<(Vec3, Vec3, usize)> {
    let count = match theme.beam_style {
        BeamStyle::Arch | BeamStyle::Ants => 16_u16,
        _ => 1,
    };
    let point = |t: f32| theme.beam_style.point(from, to, t);
    let mut segments: Vec<_> = (0..count)
        .map(|i| {
            (
                point(f32::from(i) / f32::from(count)),
                point(f32::from(i + 1) / f32::from(count)),
                usize::from(i),
            )
        })
        .collect();
    if matches!(theme.beam_style, BeamStyle::Construction) {
        let tick = (to - from).cross(Vec3::Y).normalize_or(Vec3::X) * 0.13;
        segments.extend([(from - tick, from + tick, 0), (to - tick, to + tick, 0)]);
    }
    segments
}

pub(super) fn sync_territories(
    world: &mut World,
    palette: &Palette,
    hues: &[usize],
    simulation: &Simulation,
) {
    let theme = simulation.theme();
    world.resource_scope(|world, mut styles: Mut<Styles>| {
        territories(world, palette, hues, simulation, &mut styles, theme);
    });
}

fn territories(
    world: &mut World,
    palette: &Palette,
    hues: &[usize],
    simulation: &Simulation,
    styles: &mut Styles,
    theme: &Theme,
) {
    let mut used = 0;
    if theme.territories {
        styles.territory_layout.update(simulation.layout());
        let mesh = palette.disc.clone();
        let material = styles.territories[0].clone();
        let dirs: Vec<_> = simulation
            .layout()
            .dirs()
            .filter(|(_, dir)| {
                dir.parent.is_some_and(|parent| {
                    simulation.layout().dir(parent).parent.is_none()
                })
            })
            .collect();
        grow(
            world,
            &mut styles.territory_pool,
            dirs.len() * 3,
            &mesh,
            &material,
        );
        for (index, dir) in dirs {
            let slot = hues[index] % 12;
            let radius = styles.territory_layout.radii[index] + 0.5;
            for ring in 0..3 {
                let entity = styles.territory_pool[used];
                used += 1;
                set_material(world, entity, &styles.territories[slot * 3 + ring]);
                let meshes = [&mesh, &styles.territory_mesh, &styles.territory_mesh];
                world
                    .entity_mut(entity)
                    .insert(Mesh3d(meshes[ring].clone()));
                *world
                    .get_mut::<Transform>(entity)
                    .expect("territory transform") = Transform::from_translation(
                    super::ground(dir.position)
                        - Vec3::Y
                            * (0.14
                                + f32::from(u16::try_from(ring).expect("3 rings"))
                                    * 0.005),
                )
                .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
                .with_scale(Vec3::splat(
                    radius / theme.avatar_radius
                        * (1.0
                            + f32::from(u16::try_from(ring).expect("3 rings")) * 0.07),
                ));
                set_visible(world, entity, true);
                let (grey, alpha, rise) = simulation.repository_effect(dir.repository);
                world
                    .get_mut::<Transform>(entity)
                    .expect("territory transform")
                    .translation
                    .y += rise;
                super::fade(world, entity, grey, alpha);
            }
        }
    }
    hide_rest(world, &styles.territory_pool, used);
}

pub(super) fn leaders(
    world: &mut World,
    points: &[(Vec2, f32)],
    theme: &Theme,
    camera: Entity,
) {
    world.resource_scope(|world, mut styles: Mut<Styles>| {
        let count = if theme.label_style == crate::theme::LabelStyle::Leader {
            points.len()
        } else {
            0
        };
        while styles.leaders.len() < count {
            styles.leaders.push(
                world
                    .spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            width: px(18.0),
                            height: px(1.0),
                            ..default()
                        },
                        BackgroundColor(theme.label),
                        UiTargetCamera(camera),
                        Visibility::Hidden,
                    ))
                    .id(),
            );
        }
        for (&entity, (point, opacity)) in styles.leaders.iter().zip(points).take(count)
        {
            let mut node = world.get_mut::<Node>(entity).expect("leader node");
            node.left = px(point.x - 18.0);
            node.top = px(point.y + 10.0);
            world
                .get_mut::<BackgroundColor>(entity)
                .expect("leader colour")
                .0 = theme.label.with_alpha(theme.label.alpha() * opacity);
            set_visible(world, entity, true);
        }
        hide_rest(world, &styles.leaders, count);
    });
}

pub(super) fn beams(
    world: &mut World,
    palette: &Palette,
    pools: &mut super::Pools,
    simulation: &Simulation,
) {
    let arrivals: Vec<_> = simulation
        .file_arrival_beams()
        .filter(|(dir, _)| {
            pools
                .lod
                .detail
                .get(*dir)
                .is_some_and(|detail| *detail > 0.0)
        })
        .map(|(_, beam)| beam)
        .chain(pools.directory_arrival_beams.iter().copied())
        .collect();
    let targets: std::collections::BTreeSet<_> = arrivals
        .iter()
        .map(|beam| (beam.author, beam.target))
        .collect();
    let beams = simulation.beams();
    let segments: Vec<_> = beams
        .iter()
        .filter(|beam| !targets.contains(&(beam.author, beam.target)))
        .chain(&arrivals)
        .flat_map(|beam| {
            beam_segments(simulation.theme(), beam.from, beam.to)
                .into_iter()
                .map(move |(from, to, segment)| (beam, from, to, segment))
        })
        .collect();
    let mut pool = std::mem::take(&mut pools.beams);
    super::grow(
        world,
        &mut pool,
        segments.len(),
        &palette.beam,
        &palette.glass[0],
    );
    for (entity, (beam, from, to, segment)) in pool.iter().zip(&segments) {
        let (mesh, material) = if matches!(
            simulation.theme().beam_style,
            crate::theme::BeamStyle::Straight
        ) {
            (
                &palette.beam,
                palette.glass[beam.author % palette.glass.len()].clone(),
            )
        } else {
            (
                &palette.segment_beams[*segment],
                world.resource::<Styles>().beams[*segment].clone(),
            )
        };
        super::set_mesh(world, *entity, mesh);
        world.entity_mut(*entity).insert(MeshMaterial3d(material));
        place_beam(
            world,
            *entity,
            *from,
            *to,
            beam.width * simulation.scale().sqrt() * simulation.theme().beam_width(),
        );
    }
    hide_rest(world, &pool, segments.len());
    pools.beams = pool;
}

#[cfg(test)]
mod tests {
    use super::super::{DiffSample, Playback, Pools, SceneTarget, Title};
    use super::*;
    use crate::theme::ThemeName;
    use bevy::{
        ecs::system::RunSystemOnce,
        text::{
            FontAtlas, FontAtlasKey, FontAtlasSet, FontSmoothing, GlyphAtlasInfo,
            PositionedGlyph, TextLayoutInfo,
        },
    };

    #[test]
    fn only_atlases_of_loaded_or_laid_out_fonts_survive() {
        let mut world = World::new();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<FontAtlasSet>();
        world.init_resource::<FontAtlasBudget>();
        world.init_resource::<Messages<AppExit>>();
        let font = Font::from_bytes(Vec::new());
        let loaded = u32::try_from(font.data.id()).unwrap();
        let mut fonts = Assets::<Font>::default();
        fonts.add(font);
        world.insert_resource(fonts);
        let [laid_out, stale] = [loaded + 1, loaded + 2];
        let mut used = None;
        for id in [loaded, laid_out, stale] {
            let atlas = FontAtlas::new(
                &mut world.resource_mut::<Assets<Image>>(),
                UVec2::splat(4),
                FontSmoothing::default(),
            );
            if id == laid_out {
                used = Some(atlas.texture.id());
            }
            let key = FontAtlasKey {
                id,
                index: 0,
                font_size_bits: 14.0_f32.to_bits(),
                variations_hash: 0,
                hinting: default(),
                font_smoothing: FontSmoothing::default(),
            };
            world
                .resource_mut::<FontAtlasSet>()
                .insert(key, vec![atlas]);
        }
        world.spawn(TextLayoutInfo {
            glyphs: vec![PositionedGlyph {
                position: Vec2::ZERO,
                atlas_info: GlyphAtlasInfo {
                    texture: used.unwrap(),
                    rect: Rect::default(),
                    offset: Vec2::ZERO,
                    is_alpha_mask: true,
                },
                section_index: 0,
                line_index: 0,
            }],
            ..default()
        });
        world.init_resource::<ScaleCx>();
        world.spawn((
            ComputedNode::default(),
            TextLayout::default(),
            ComputedTextBlock::default(),
            FontHinting::default(),
            TextNodeFlags::default(),
        ));
        world.run_system_once(reserve_font_atlases).unwrap();
        let mut kept: Vec<_> = world
            .resource::<FontAtlasSet>()
            .keys()
            .map(|key| key.id)
            .collect();
        kept.sort_unstable();
        assert_eq!(kept, [loaded, laid_out]);
    }

    #[test]
    fn reservation_reclaims_stale_fallback_pages_at_the_limit() {
        use bevy::text::{
            FontCx, LayoutCx, LetterSpacing, LineHeight, RemSize, TextBounds,
            TextElement, TextPipeline,
        };

        let mut world = World::new();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<Font>>();
        world.init_resource::<FontAtlasBudget>();
        world.init_resource::<ScaleCx>();
        world.init_resource::<Messages<AppExit>>();
        let font = Font::from_bytes(
            include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf").to_vec(),
        );
        let font = TextFont::from_font_size(14.0)
            .with_font(world.resource_mut::<Assets<Font>>().add(font));
        let stale = FontAtlasKey {
            id: u32::MAX,
            index: 0,
            font_size_bits: 14.0_f32.to_bits(),
            variations_hash: 0,
            hinting: default(),
            font_smoothing: FontSmoothing::default(),
        };
        let pages = (0..MAX_FONT_ATLAS_PAGES)
            .map(|_| {
                FontAtlas::new(
                    &mut world.resource_mut::<Assets<Image>>(),
                    UVec2::splat(4),
                    stale.font_smoothing,
                )
            })
            .collect();
        let mut atlases = FontAtlasSet::default();
        atlases.insert(stale, pages);
        world.insert_resource(atlases);
        let mut computed = ComputedTextBlock::default();
        TextPipeline::default()
            .update_buffer(
                world.resource::<Assets<Font>>(),
                [(
                    Entity::PLACEHOLDER,
                    0,
                    TextElement::Text {
                        text: "A",
                        font: &font,
                        color: Color::WHITE,
                        line_height: LineHeight::default(),
                        letter_spacing: LetterSpacing::default(),
                    },
                )]
                .into_iter(),
                LineBreak::NoWrap,
                Justify::Left,
                TextBounds::UNBOUNDED,
                1.0,
                &mut computed,
                &mut FontCx::default(),
                &mut LayoutCx::default(),
                Vec2::new(1280.0, 720.0),
                RemSize::default(),
            )
            .unwrap();
        let font_id = u32::try_from(
            computed
                .buffer()
                .lines()
                .next()
                .unwrap()
                .runs()
                .next()
                .unwrap()
                .font()
                .data
                .id(),
        )
        .unwrap();
        world.spawn((
            ComputedNode::default(),
            TextLayout::new(Justify::Left, LineBreak::NoWrap),
            computed,
            FontHinting::default(),
            TextNodeFlags::default(),
        ));
        world.run_system_once(reserve_font_atlases).unwrap();
        assert!(!world.resource::<FontAtlasBudget>().exceeded);
        let atlases = world.resource::<FontAtlasSet>();
        assert_eq!(atlases.len(), 1);
        let (key, pages) = atlases.iter().next().unwrap();
        assert_eq!(key.id, font_id);
        assert_eq!(pages.len(), 1);
        assert_eq!(world.resource::<Assets<Image>>().len(), 1);
    }

    #[test]
    fn font_page_budget_stops_before_allocating_the_next_image() {
        let mut textures = Assets::<Image>::default();
        let mut atlases = FontAtlasSet::default();
        let mut scale = ScaleCx::default();
        let data = include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf");
        let font = Font::from_bytes(data.to_vec());
        let data_id = font.data.id();
        let charmap = FontRef::from_index(data, 0).unwrap().charmap();
        let key = FontAtlasKey {
            id: u32::try_from(data_id).unwrap(),
            index: 0,
            font_size_bits: 14.0_f32.to_bits(),
            variations_hash: FixedHasher.hash_one(&[] as &[i16]),
            hinting: default(),
            font_smoothing: FontSmoothing::default(),
        };
        let pages = (1..MAX_FONT_ATLAS_PAGES)
            .map(|_| FontAtlas::new(&mut textures, UVec2::splat(4), key.font_smoothing))
            .collect();
        atlases.insert(
            FontAtlasKey {
                id: key.id + 1,
                ..key
            },
            pages,
        );
        let mut reservation = AtlasReservation {
            pages: MAX_FONT_ATLAS_PAGES - 1,
            textures: &mut textures,
            atlases: &mut atlases,
            scale: &mut scale,
        };
        assert!(reservation.glyph(key, charmap.map('A'), data, data_id, &[]));
        assert!(reservation.glyph(key, charmap.map('B'), data, data_id, &[]));
        let other_size = FontAtlasKey {
            font_size_bits: 24.0_f32.to_bits(),
            ..key
        };
        assert!(!reservation.glyph(other_size, charmap.map('A'), data, data_id, &[]));
        assert_eq!(reservation.textures.len(), MAX_FONT_ATLAS_PAGES);
        assert!(reservation.atlases.get(&other_size).is_none());
    }

    fn active_code_world() -> World {
        let mut events = crate::timeline::synthetic();
        events.truncate(1);
        events[0].changes.truncate(1);
        events[0].changes[0].insertions = 7;
        events[0].changes[0].removals = 7;
        events[0].changes[0].samples = [true, false]
            .map(|added| crate::diff_samples::Sample {
                added,
                text: "let code = ascii();".to_owned(),
            })
            .to_vec();
        let mut simulation = Simulation::new(
            events,
            &crate::model::Options::default(),
            &Theme::neon(),
            &crate::contributors::Mappings::default(),
        );
        simulation.advance_to(45, 60);
        let mut world = World::new();
        world.insert_resource(SceneTarget(Handle::<Image>::default().into()));
        world.insert_resource(Title("test".to_owned()));
        world.insert_resource(Playback(simulation));
        world.init_resource::<Pools>();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<Assets<StandardMaterial>>();
        super::super::setup(&mut world);
        world
    }

    #[test]
    fn switching_themes_restyles_active_code_without_resetting_fragments() {
        let mut world = active_code_world();
        let seconds = world.resource::<Playback>().0.seconds();
        let lines = world.resource::<Playback>().0.diff_lines();
        let extent = 0.07 * lines[0].scale * 500.0 / 48.0;
        let camera = Camera {
            computed: bevy::camera::ComputedCameraValues {
                clip_from_view: bevy::math::proj::orthographic(
                    -extent, extent, -extent, extent, 0.1, 100.0,
                ),
                target_info: Some(bevy::camera::RenderTargetInfo {
                    physical_size: UVec2::splat(1000),
                    scale_factor: 1.0,
                }),
                ..default()
            },
            ..default()
        };
        for name in ThemeName::ALL.into_iter().chain([ThemeName::Neon]) {
            super::super::switch_theme(&mut world, name);
            let theme = name.build();
            assert_eq!(world.resource::<Playback>().0.seconds(), seconds);
            let current = world.resource::<Playback>().0.diff_lines();
            assert_eq!(current[0].scale, lines[0].scale);
            assert_eq!(current[0].alpha, lines[0].alpha);
            let anchor = current[0].position;
            let mut pools = world.remove_resource::<Pools>().unwrap();
            pools.lod.detail =
                vec![1.0; world.resource::<Playback>().0.layout().dir_count()];
            world.resource_scope(|world, playback: Mut<Playback>| {
                world.resource_scope(|world, palette: Mut<Palette>| {
                    super::super::sync_activity(
                        world,
                        &palette,
                        &mut pools,
                        &playback.0,
                    );
                });
            });
            world.insert_resource(pools);
            super::super::diff_samples(
                &mut world,
                &camera,
                &GlobalTransform::from_translation(anchor + Vec3::Z * 10.0),
            );
            let kind = if theme.font == FontStyle::Serif {
                FontStyle::Plex
            } else {
                theme.font
            };
            let styles = world.resource::<Styles>();
            let expected = styles
                .fonts
                .iter()
                .find(|(style, _)| *style == kind)
                .map(|(_, handle)| handle.clone())
                .unwrap_or_default();
            let fallback = styles
                .fonts
                .iter()
                .find(|(kind, _)| *kind == FontStyle::Plex)
                .unwrap()
                .1
                .clone();
            let expected = label_font(expected, fallback);
            let mut visible = 0;
            for (text, font, color, shadow, visibility, transform) in world
                .query_filtered::<(
                    &Text,
                    &TextFont,
                    &TextColor,
                    &TextShadow,
                    &Visibility,
                    &UiTransform,
                ), With<DiffSample>>()
                .iter(&world)
            {
                assert_eq!(font.font, expected);
                if *visibility == Visibility::Hidden {
                    continue;
                }
                visible += 1;
                let ink = if text.0.starts_with('+') {
                    theme.diff_added
                } else {
                    theme.diff_removed
                };
                assert_eq!(color.0.to_srgba().red, ink.to_srgba().red);
                assert_eq!(transform.scale.x, transform.scale.y);
                assert!(
                    (shadow.color.alpha()
                        - theme.label_shadow.color.alpha() * color.0.alpha())
                    .abs()
                        < 0.0001
                );
            }
            assert!(visible > 0, "{}", name.title());
        }
    }

    fn reference_owner(layout: &crate::layout::Layout, mut owner: usize) -> usize {
        while let Some(parent) = layout.dir(owner).parent.filter(|&parent| parent != 0)
        {
            owner = parent;
        }
        owner
    }

    #[test]
    fn territories_keep_ownership_inside_each_repository() {
        let histories = (0..2)
            .map(|repository| {
                let mut events = crate::timeline::synthetic();
                crate::repositories::namespace(&mut events, repository);
                events
            })
            .collect();
        let mut simulation = Simulation::with_repositories(
            crate::repositories::merge(histories),
            &crate::model::Options::default(),
            &ThemeName::Atlas.build(),
            &crate::contributors::Mappings::default(),
            &["first".into(), "second".into()],
        );
        simulation.advance_to(
            crate::model::frames_for(simulation.duration(), 60).unwrap(),
            60,
        );
        let layout = simulation.layout();
        let mut cache = Territories::default();
        cache.update(layout);
        for (_, file) in layout.files() {
            let mut expected = file.dir;
            while let Some(parent) = layout
                .dir(expected)
                .parent
                .filter(|&parent| layout.dir(parent).parent.is_some())
            {
                expected = parent;
            }
            assert_eq!(cache.owners[file.dir], expected);
            assert_eq!(
                layout.dir(expected).repository,
                layout.dir(file.dir).repository
            );
            if layout.dir(expected).parent.is_some() {
                assert!(
                    cache.radii[expected]
                        >= file.position.distance(layout.dir(expected).position)
                );
            }
        }
        assert_eq!(
            layout
                .dirs()
                .filter(|(_, dir)| dir.parent.is_none())
                .count(),
            2
        );
    }
    #[test]
    fn territory_cache_tracks_nested_moves_and_reuses_unchanged_layouts() {
        let mut layout = crate::layout::Layout::planned_paths(
            &["a/deep/one.rs", "b/two.rs", "root.rs"],
            0,
        );
        for path in ["a/deep/one.rs", "b/two.rs", "root.rs"] {
            layout.add(path, 0.0, 0);
        }
        layout.settle(10, 1.0 / 60.0);
        let mut cache = Territories::default();
        for moved in [false, true] {
            if moved {
                layout.rename("a/deep/one.rs", "b/one.rs", 0);
                layout.settle(10, 1.0 / 60.0);
            }
            cache.update(&layout);
            for (index, dir) in layout.dirs().filter(|(_, d)| d.parent == Some(0)) {
                let expected = layout
                    .files()
                    .filter(|(_, f)| {
                        let owner = reference_owner(&layout, f.dir);
                        owner == index
                    })
                    .map(|(_, f)| f.position.distance(dir.position))
                    .fold(1.0_f32, f32::max);
                assert_eq!(cache.radii[index], expected);
            }
            cache.radii[0] = 123.0;
            cache.update(&layout);
            assert_eq!(cache.radii[0], 123.0);
        }
    }

    #[test]
    fn styled_beams_keep_endpoints_and_static_dash_positions() {
        let from = Vec3::new(-2.0, 3.0, 1.0);
        let to = Vec3::new(4.0, 0.0, -1.0);
        for name in ThemeName::ALL {
            let theme = name.build();
            let segments = beam_segments(&theme, from, to);
            let count = match theme.beam_style {
                BeamStyle::Arch | BeamStyle::Ants => 16_u16,
                _ => 1,
            };
            for (i, (a, b, _)) in (0..count).zip(&segments) {
                assert_eq!(
                    *a,
                    theme
                        .beam_style
                        .point(from, to, f32::from(i) / f32::from(count))
                );
                assert_eq!(
                    *b,
                    theme.beam_style.point(
                        from,
                        to,
                        f32::from(i + 1) / f32::from(count)
                    )
                );
            }
            assert_eq!(segments[0].0, from);
            let endpoint = if matches!(theme.beam_style, BeamStyle::Construction) {
                0
            } else {
                segments.len() - 1
            };
            assert_eq!(segments[endpoint].1, to);
            assert_eq!(segments, beam_segments(&theme, from, to));
            assert!(
                segments
                    .iter()
                    .all(|(a, b, _)| a.is_finite() && b.is_finite())
            );
            assert!(
                beam_segments(&theme, from, from)
                    .iter()
                    .all(|(a, b, _)| a.is_finite() && b.is_finite())
            );
        }
        let arch = beam_segments(&ThemeName::Outrun.build(), from, to);
        assert!(arch[7].1.y > from.lerp(to, 0.5).y);
    }
}
