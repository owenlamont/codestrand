use super::{Theme, ThemeName};
use bevy::color::Luminance;
use bevy::prelude::*;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum GlyphStyle {
    #[default]
    Language,
    Outline,
    Hatched,
    Page,
    Survey,
    Iron,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum BeamStyle {
    #[default]
    Straight,
    Arch,
    Construction,
    Ants,
    Ink,
    Hot,
}

impl BeamStyle {
    pub(crate) fn point(self, from: Vec3, to: Vec3, t: f32) -> Vec3 {
        let height = match self {
            Self::Arch => 4.0 * t * (1.0 - t) * from.distance(to) * 0.25,
            Self::Straight
            | Self::Construction
            | Self::Ants
            | Self::Ink
            | Self::Hot => 0.0,
        };
        from.lerp(to, t) + Vec3::Y * height
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) enum AvatarStyle {
    #[default]
    Sphere,
    Reticle,
    Pointer,
    Pin,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum FontStyle {
    #[default]
    Default,
    Tech,
    Serif,
    Plex,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum LabelStyle {
    #[default]
    Plain,
    Leader,
    Bevelled,
    SmallCaps,
}

pub(super) fn color(hex: u32) -> Color {
    let [_, r, g, b] = hex.to_be_bytes();
    Color::srgb_u8(r, g, b)
}

fn solid(hex: u32) -> StandardMaterial {
    StandardMaterial {
        base_color: color(hex),
        unlit: true,
        ..default()
    }
}

pub(super) fn build(name: ThemeName) -> Theme {
    let mut t = Theme::neon();
    t.name = name;
    t.heat_gain = 0.0;
    let (ground, ink, accent, fragment, hues) = match name {
        ThemeName::Outrun => {
            t.glyph_style = GlyphStyle::Outline;
            t.beam_style = BeamStyle::Arch;
            t.font = FontStyle::Tech;
            t.heat_gain = 1.5;
            (
                0x0012_022a,
                0x00ff_e9f6,
                0x00ff_3fa4,
                0x00ff_f3b0,
                [0x00ff_3fa4, 0x00ff_b347, 0x008c_6cff, 0x003f_f0ff],
            )
        }
        ThemeName::Blueprint => {
            t.glyph_style = GlyphStyle::Hatched;
            t.beam_style = BeamStyle::Construction;
            t.avatar_style = AvatarStyle::Reticle;
            t.font = FontStyle::Tech;
            t.edge_dash = Some(0.6);
            t.label_style = LabelStyle::Leader;
            (
                0x000d_3a73,
                0x00ff_ffff,
                0x00ff_d84a,
                0x00ff_d84a,
                [0x00ff_ffff; 4],
            )
        }
        ThemeName::BevelledDesktop => {
            t.glyph_style = GlyphStyle::Page;
            t.beam_style = BeamStyle::Ants;
            t.avatar_style = AvatarStyle::Pointer;
            t.label_style = LabelStyle::Bevelled;
            (
                0x000f_7b7b,
                0x0000_0000,
                0x00ff_ffff,
                0x00ff_ffff,
                [0x0000_0080, 0x0080_0000, 0x0000_8000, 0x0080_8000],
            )
        }
        ThemeName::Atlas => {
            t.glyph_style = GlyphStyle::Survey;
            t.beam_style = BeamStyle::Ink;
            t.avatar_style = AvatarStyle::Pin;
            t.font = FontStyle::Serif;
            t.territories = true;
            t.label_style = LabelStyle::SmallCaps;
            (
                0x00ec_eee6,
                0x0028_352f,
                0x00b5_562b,
                0x00b5_562b,
                [0x002f_6f8f, 0x00b5_562b, 0x006c_4f9e, 0x003f_7d3a],
            )
        }
        ThemeName::Forge => {
            t.glyph_style = GlyphStyle::Iron;
            t.beam_style = BeamStyle::Hot;
            t.font = FontStyle::Plex;
            t.blackbody = true;
            t.glyph_material.base_color = color(0x002a_2422);
            t.glyph_material.unlit = false;
            t.glyph_material.emissive = LinearRgba::from(color(0x002a_2422));
            (
                0x0007_0505,
                0x00d9_cfc6,
                0x00ff_faeb,
                0x00ff_d27a,
                [0x005a_4a42; 4],
            )
        }
        ThemeName::Neon | ThemeName::Phosphor => unreachable!("candidate theme"),
    };
    apply_palette(&mut t, ground, ink, accent, fragment, hues);
    let (added, removed) = match name {
        ThemeName::Outrun => (0x0066_ffaa, 0x00ff_709e),
        ThemeName::Blueprint => (0x008e_ffca, 0x00ff_b4a6),
        ThemeName::BevelledDesktop => (0x00d8_ffdf, 0x00ff_efef),
        ThemeName::Atlas => (0x001c_643b, 0x00a1_292d),
        ThemeName::Forge => (0x008c_e8aa, 0x00ff_8a78),
        ThemeName::Neon | ThemeName::Phosphor => unreachable!("candidate theme"),
    };
    t.diff_added = color(added);
    t.diff_removed = color(removed);
    t
}

fn apply_palette(
    t: &mut Theme,
    ground: u32,
    ink: u32,
    accent: u32,
    fragment: u32,
    hues: [u32; 4],
) {
    t.clear = color(ground);
    t.floor = solid(0x00ff_ffff);
    t.textured_ground = true;
    t.grid = solid(ground);
    t.edge = solid(hues[0]);
    t.directories = (0..12).map(|i| vec![solid(hues[i % 4]); 4]).collect();
    t.directory_edge_widths = vec![0.055, 0.045, 0.035, 0.025];
    t.label = color(ink);
    t.heading = color(ink);
    t.label_emphasis = color(ink);
    t.label_landmark = color(ink);
    t.label_shadow.color = color(ground).with_alpha(0.8);
    if t.name == ThemeName::Atlas || t.name == ThemeName::BevelledDesktop {
        t.label_shadow.color = Color::NONE;
    }
    t.descending = solid(fragment);
    t.rising = solid(fragment);
    for (i, (avatar, glass)) in t.avatars.iter_mut().zip(&mut t.glass).enumerate() {
        *avatar = solid(if t.name == ThemeName::Outrun {
            hues[i % 4]
        } else {
            accent
        });
        *glass = StandardMaterial {
            base_color: color(accent).with_alpha(0.85),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        };
    }
}

pub(super) const BLACKBODY: [(f32, u32); 6] = [
    (0.0, 0x002a_2422),
    (0.3, 0x0078_180a),
    (0.55, 0x00dc_460c),
    (0.75, 0x00ff_961e),
    (0.9, 0x00ff_dc78),
    (1.0, 0x00ff_faeb),
];

impl Theme {
    pub(crate) fn territory_material(
        &self,
        slot: usize,
        contour: bool,
    ) -> StandardMaterial {
        let mut material = self.directories[slot % self.directories.len()][0].clone();
        material
            .base_color
            .set_alpha(if contour { 0.22 } else { 0.06 });
        material.alpha_mode = AlphaMode::Blend;
        material
    }

    pub(crate) fn beam_material(&self, segment: u16) -> StandardMaterial {
        let color = match self.beam_style {
            BeamStyle::Arch => Color::srgb(
                1.0,
                0.25 + 0.45 * f32::from(segment) / 15.0,
                0.65 - 0.37 * f32::from(segment) / 15.0,
            ),
            BeamStyle::Construction => Color::WHITE,
            BeamStyle::Ants => {
                if segment.is_multiple_of(2) {
                    Color::BLACK
                } else {
                    Color::WHITE
                }
            }
            BeamStyle::Ink => self.label,
            BeamStyle::Hot => color(0x00ff_faeb),
            BeamStyle::Straight => self.glass[0].base_color,
        };
        StandardMaterial {
            base_color: color,
            unlit: true,
            ..default()
        }
    }

    pub(crate) fn label_background(&self) -> Color {
        if self.label_style == LabelStyle::Bevelled {
            color(0x00c0_c0c0)
        } else {
            Color::NONE
        }
    }

    pub(crate) fn detail_label_background(&self) -> Color {
        let background = self.label_background();
        if background.alpha() == 0.0
            && self.label_emphasis.to_linear().luminance() > 0.5
        {
            Color::srgba(0.015, 0.02, 0.04, 0.85)
        } else {
            background
        }
    }

    pub(crate) fn label_border(&self) -> BorderColor {
        if self.label_style != LabelStyle::Bevelled {
            return BorderColor::all(Color::NONE);
        }
        BorderColor {
            top: Color::WHITE,
            left: Color::WHITE,
            bottom: color(0x0050_5050),
            right: color(0x0050_5050),
        }
    }

    pub(crate) fn beam_width(&self) -> f32 {
        match self.beam_style {
            BeamStyle::Construction | BeamStyle::Ink => 0.4,
            _ => 1.0,
        }
    }

    pub(crate) fn bloom(&self) -> Option<bevy::post_process::bloom::Bloom> {
        self.blackbody.then_some(bevy::post_process::bloom::Bloom {
            intensity: 0.12,
            composite_mode: bevy::post_process::bloom::BloomCompositeMode::Additive,
            prefilter: bevy::post_process::bloom::BloomPrefilter {
                threshold: LinearRgba::from(self.heat_color(0.3)).red,
                threshold_softness: 0.0,
            },
            ..bevy::post_process::bloom::Bloom::NATURAL
        })
    }

    pub(crate) fn glyph_svg(&self, source: &str, kind: usize) -> String {
        let art = match self.glyph_style {
            GlyphStyle::Language => return source.to_owned(),
            GlyphStyle::Outline => r##"<rect x="3" y="3" width="26" height="26" rx="2" fill="#310b39" stroke="#ff3fa4" stroke-width="2"/><path d="M7 24V8h18M7 26h18" fill="none" stroke="#ff8dce"/>"##.to_owned(),
            GlyphStyle::Hatched => r#"<defs><pattern id="h" width="5" height="5" patternUnits="userSpaceOnUse"><path d="M0 5L5 0" stroke="white" stroke-width=".6"/></pattern></defs><rect x="3" y="3" width="26" height="26" fill="url(#h)" stroke="white" stroke-width="1.5"/>"#.to_owned(),
            GlyphStyle::Page => {
                let tint = ["#b7410e", "#3776ab", "#e6b800", "#764abc", "#00843d", "#c04389"][kind % 6];
                format!(r##"<path d="M5 2H21L28 9V30H5Z" fill="white" stroke="#202020"/><path d="M21 2V9H28" fill="#c0c0c0" stroke="#202020"/><path d="M7 13H25V18H7Z" fill="{tint}"/><path d="M8 22H23M8 25H20" stroke="#808080"/>"##)
            }
            GlyphStyle::Survey => r##"<circle cx="16" cy="16" r="8" fill="#eceee6" stroke="#28352f" stroke-width="2"/><circle cx="16" cy="16" r="2" fill="#28352f"/><path d="M16 2V8M16 24V30M2 16H8M24 16H30" stroke="#28352f" stroke-width="1.5"/>"##.to_owned(),
            GlyphStyle::Iron => r##"<rect x="2" y="3" width="28" height="26" rx="2" fill="white"/><path d="M4 27V5H28" fill="none" stroke="#999999" stroke-width="2"/><path d="M6 8H8M24 8H26M6 24H8M24 24H26" stroke="#333333" stroke-width="2"/>"##.to_owned(),
        };
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32">{art}</svg>"#
        )
    }

    pub(crate) fn heat_emissive(&self, heat: f32) -> LinearRgba {
        if self.blackbody {
            LinearRgba::from(self.heat_color(heat))
                * (1.0 + (heat - 0.3).max(0.0) * 4.0)
        } else {
            LinearRgba::BLACK
        }
    }
}
