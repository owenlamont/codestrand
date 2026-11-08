use bevy::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Geometry {
    pub file_min: Vec3,
    pub file_max: Vec3,
    pub avatar_radius: f32,
    pub avatar_label_offset: Vec3,
    pub label_height: f32,
}
mod candidates;
mod ground;
mod icons;
pub(crate) use candidates::{
    AvatarStyle, BeamStyle, FontStyle, GlyphStyle, LabelStyle,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub(super) enum ThemeName {
    #[default]
    Neon,
    Phosphor,
    Outrun,
    Blueprint,
    BevelledDesktop,
    Atlas,
    Forge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ThemeDirection {
    Previous,
    Next,
}

impl ThemeDirection {
    pub(super) fn index(self, index: usize, count: usize) -> usize {
        match self {
            Self::Previous => (index + count - 1) % count,
            Self::Next => (index + 1) % count,
        }
    }
}

impl ThemeName {
    pub(super) const ALL: [Self; 7] = [
        Self::Neon,
        Self::Phosphor,
        Self::Outrun,
        Self::Blueprint,
        Self::BevelledDesktop,
        Self::Atlas,
        Self::Forge,
    ];

    pub(super) fn cycle(self, direction: ThemeDirection) -> Self {
        let index = Self::ALL
            .iter()
            .position(|name| *name == self)
            .expect("every theme name is registered");
        Self::ALL[direction.index(index, Self::ALL.len())]
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Neon => "Neon",
            Self::Phosphor => "Phosphor",
            Self::Outrun => "Outrun",
            Self::Blueprint => "Blueprint",
            Self::BevelledDesktop => "Bevelled Desktop",
            Self::Atlas => "Atlas",
            Self::Forge => "Forge",
        }
    }

    pub(super) fn build(self) -> Theme {
        match self {
            Self::Neon => Theme::neon(),
            Self::Phosphor => Theme::phosphor(),
            Self::Outrun
            | Self::Blueprint
            | Self::BevelledDesktop
            | Self::Atlas
            | Self::Forge => candidates::build(self),
        }
    }
}

#[derive(Clone)]
pub(super) struct Theme {
    pub(crate) glyph_style: GlyphStyle,
    pub(crate) beam_style: BeamStyle,
    pub(crate) avatar_style: AvatarStyle,
    pub(crate) font: FontStyle,
    pub(crate) blackbody: bool,
    pub(crate) textured_ground: bool,
    pub(crate) edge_dash: Option<f32>,
    pub(crate) label_style: LabelStyle,
    pub(crate) territories: bool,
    pub(super) name: ThemeName,
    pub(super) glyph_ink: Option<[u8; 3]>,
    pub(super) glyphs: &'static [FileGlyph],
    pub(super) glyph_material: StandardMaterial,
    pub(super) heat_gain: f32,
    pub(super) glyph_texture: GlyphTexture,
    pub(super) file_glyph_transform: Transform,
    pub(super) file_tile: Option<FileTile>,
    pub(super) membership_radii: Vec2,
    pub(super) membership_transform: Transform,
    pub(super) activity_radii: Vec2,
    pub(super) activity_transform: Transform,
    pub(super) activity_growth: f32,
    pub(super) clear: Color,
    pub(super) edge: StandardMaterial,
    pub(super) grid: StandardMaterial,
    pub(super) descending: StandardMaterial,
    pub(super) rising: StandardMaterial,
    pub(super) floor: StandardMaterial,
    pub(super) avatars: [StandardMaterial; 6],
    pub(super) glass: [StandardMaterial; 6],
    pub(super) directories: Vec<Vec<StandardMaterial>>,
    pub(super) directory_node_scale: Vec3,
    pub(super) directory_edge_widths: Vec<f32>,
    pub(super) directory_edge_height: f32,
    pub(super) label: Color,
    pub(super) diff_added: Color,
    pub(super) diff_removed: Color,
    pub(super) label_shadow: TextShadow,
    pub(super) heading: Color,
    pub(super) label_emphasis: Color,
    pub(super) label_landmark: Color,
    pub(super) avatar_radius: f32,
    pub(super) avatar_label_offset: Vec3,
    pub(super) label_height: f32,
}

pub(super) struct FileGlyph {
    pub(super) svg: &'static str,
    filenames: Vec<String>,
    extensions: Vec<String>,
}

#[derive(Clone, Copy)]
pub(super) struct GlyphTexture {
    pub(super) size: u16,
    pub(super) mipmaps: bool,
    pub(super) padding: f32,
}

#[derive(Clone)]
pub(super) struct FileTile {
    pub(super) material: StandardMaterial,
    pub(super) transform: Transform,
}

impl Theme {
    pub(super) fn heat_color(&self, heat: f32) -> Color {
        if self.blackbody {
            let heat = heat.clamp(0.0, 1.0);
            for pair in candidates::BLACKBODY.windows(2) {
                let [(low, from), (high, to)] = [pair[0], pair[1]];
                if heat <= high {
                    let from = Srgba::from(candidates::color(from));
                    let to = Srgba::from(candidates::color(to));
                    let t = (heat - low) / (high - low);
                    return Color::srgb(
                        from.red + (to.red - from.red) * t,
                        from.green + (to.green - from.green) * t,
                        from.blue + (to.blue - from.blue) * t,
                    );
                }
            }
        }
        let base = LinearRgba::from(self.glyph_material.base_color);
        let brightness = 1.0 + self.heat_gain * heat.clamp(0.0, 1.0);
        Color::linear_rgba(
            base.red * brightness,
            base.green * brightness,
            base.blue * brightness,
            base.alpha,
        )
    }

    pub(super) fn neon() -> Self {
        let developers = [
            Color::srgb(0.25, 0.92, 0.95),
            Color::srgb(1.0, 0.6, 0.25),
            Color::srgb(0.95, 0.35, 0.85),
            Color::srgb(0.55, 1.0, 0.45),
            Color::srgb(1.0, 0.9, 0.35),
            Color::srgb(0.55, 0.6, 1.0),
        ];
        let solid = |base_color| StandardMaterial {
            base_color,
            unlit: true,
            ..default()
        };
        Self {
            glyph_style: GlyphStyle::default(),
            beam_style: BeamStyle::default(),
            avatar_style: AvatarStyle::default(),
            font: FontStyle::default(),
            blackbody: false,
            textured_ground: false,
            edge_dash: None,
            label_style: LabelStyle::default(),
            territories: false,
            name: ThemeName::Neon,
            heat_gain: 2.5,
            glyph_ink: None,
            glyphs: icons::material(),
            glyph_texture: GlyphTexture {
                size: 256,
                mipmaps: true,
                padding: 1.0 / 32.0,
            },
            glyph_material: StandardMaterial {
                base_color: Color::WHITE,
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                ..default()
            },
            file_glyph_transform: Transform::from_xyz(0.0, 0.105, 0.0)
                .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2))
                .with_scale(Vec3::new(0.48, 0.48, 1.0)),
            file_tile: None,
            membership_radii: Vec2::new(0.255, 0.27),
            membership_transform: Transform::from_xyz(0.0, 0.015, 0.0)
                .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
            activity_radii: Vec2::new(0.29, 0.32),
            activity_transform: Transform::from_xyz(0.0, 0.035, 0.0)
                .with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
            activity_growth: 0.45,
            clear: Color::srgb(0.012, 0.023, 0.04),
            edge: solid(Color::srgb(0.09, 0.2, 0.28)),
            grid: solid(Color::srgb(0.045, 0.09, 0.13)),
            descending: solid(developers[0]),
            rising: solid(developers[1]),
            floor: solid(Color::srgb(0.02, 0.036, 0.06)),
            avatars: developers.map(solid),
            glass: developers.map(|color| StandardMaterial {
                base_color: color.with_alpha(0.32),
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                ..default()
            }),
            directories: [
                200.0, 20.0, 275.0, 135.0, 335.0, 65.0, 225.0, 165.0, 305.0, 95.0,
                355.0, 245.0,
            ]
            .map(|hue| {
                [(0.65, 0.52), (0.62, 0.49), (0.58, 0.46), (0.54, 0.43)]
                    .map(|(saturation, lightness)| {
                        solid(Color::hsl(hue, saturation, lightness))
                    })
                    .to_vec()
            })
            .to_vec(),
            directory_node_scale: Vec3::new(0.24, 0.04, 0.24),
            directory_edge_widths: vec![0.20, 0.15, 0.11, 0.08],
            directory_edge_height: 0.025,
            label: Color::srgb(0.72, 0.83, 0.9),
            diff_added: Color::srgb(0.25, 0.85, 0.42),
            diff_removed: Color::srgb(0.95, 0.28, 0.32),
            label_shadow: TextShadow {
                offset: Vec2::splat(1.5),
                color: Color::srgb(0.008, 0.016, 0.03),
            },
            heading: Color::srgb(0.65, 0.8, 0.87),
            label_emphasis: Color::srgb(0.88, 0.95, 1.0),
            label_landmark: Color::srgb(0.62, 0.86, 0.95),
            avatar_radius: 0.22,
            avatar_label_offset: Vec3::new(0.3, 0.3, 0.0),
            label_height: 20.0,
        }
    }

    fn phosphor() -> Self {
        let mut theme = Self::neon();
        theme.name = ThemeName::Phosphor;
        theme.heat_gain = 3.5;
        theme.glyph_ink = Some([0x39, 0xff, 0x6a]);
        let green = Color::srgb_u8(0x39, 0xff, 0x6a);
        let dark = Color::srgb_u8(0x0b, 0x13, 0x0d);
        theme.clear = Color::BLACK;
        theme.floor.base_color = Color::BLACK;
        theme.grid.base_color = Color::srgb(0.045, 0.105, 0.055);
        theme.edge.base_color = Color::srgb(0.10, 0.32, 0.16);
        theme.descending.base_color = green;
        theme.rising.base_color = Color::srgb(0.65, 1.0, 0.72);
        let contributors = [
            Color::hsl(100.0, 0.90, 0.60),
            Color::hsl(112.0, 0.85, 0.46),
            Color::hsl(124.0, 0.95, 0.84),
            Color::hsl(136.0, 0.80, 0.54),
            Color::hsl(148.0, 0.90, 0.65),
            Color::hsl(160.0, 0.95, 0.42),
        ];
        for ((avatar, glass), color) in theme
            .avatars
            .iter_mut()
            .zip(&mut theme.glass)
            .zip(contributors)
        {
            avatar.base_color = color;
            glass.base_color = color.with_alpha(0.24);
        }
        for depths in &mut theme.directories {
            for (depth, material) in depths.iter_mut().enumerate() {
                let strength = [0.46, 0.40, 0.35, 0.30][depth.min(3)];
                material.base_color = Color::srgb(0.12, strength, 0.21);
            }
        }
        theme.heading = green;
        theme.label = Color::srgb(0.56, 0.85, 0.63);
        theme.diff_added = Color::srgb(0.45, 0.92, 0.5);
        theme.diff_removed = Color::srgb(0.85, 0.38, 0.3);
        theme.label_emphasis = Color::srgb(0.77, 1.0, 0.82);
        theme.label_landmark = Color::srgb(0.38, 0.78, 0.48);
        theme.label_shadow.color = dark;
        theme
    }

    pub(super) fn geometry(&self) -> Geometry {
        let pieces = [
            (self.file_glyph_transform, Vec3::new(0.5, 0.5, 0.0)),
            (
                self.membership_transform,
                Vec3::new(self.membership_radii.y, self.membership_radii.y, 0.0),
            ),
        ]
        .into_iter()
        .chain(
            self.file_tile
                .iter()
                .map(|tile| (tile.transform, Vec3::splat(0.5))),
        );
        let mut file_min = Vec3::splat(f32::MAX);
        let mut file_max = Vec3::splat(f32::MIN);
        for (transform, half) in pieces {
            let world_half = Mat3::from_quat(transform.rotation).abs()
                * (transform.scale * half).abs();
            file_min = file_min.min(transform.translation - world_half);
            file_max = file_max.max(transform.translation + world_half);
        }
        Geometry {
            file_min,
            file_max,
            avatar_radius: self.avatar_radius,
            avatar_label_offset: self.avatar_label_offset,
            label_height: self.label_height,
        }
    }

    pub(super) fn directory_edge_width(&self, depth: u16) -> f32 {
        self.directory_edge_widths[usize::from(depth.saturating_sub(1))
            .min(self.directory_edge_widths.len() - 1)]
    }

    pub(super) fn glyph_kind(&self, name: &str) -> usize {
        self.glyphs
            .iter()
            .position(|glyph| {
                glyph
                    .filenames
                    .iter()
                    .any(|known| known.eq_ignore_ascii_case(name))
            })
            .or_else(|| {
                name.match_indices('.').find_map(|(index, _)| {
                    let extension = &name[index + 1..];
                    self.glyphs.iter().position(|glyph| {
                        glyph
                            .extensions
                            .iter()
                            .any(|known| known.eq_ignore_ascii_case(extension))
                    })
                })
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_interpolates_the_approved_ramp_and_clamps() {
        let theme = ThemeName::Forge.build();
        for (heat, hex) in candidates::BLACKBODY {
            let actual = Srgba::from(theme.heat_color(heat));
            let expected = Srgba::from(candidates::color(hex));
            assert!((actual.red - expected.red).abs() < 0.000_001);
            assert!((actual.green - expected.green).abs() < 0.000_001);
            assert!((actual.blue - expected.blue).abs() < 0.000_001);
        }
        assert_eq!(theme.heat_color(-1.0), theme.heat_color(0.0));
        assert_eq!(theme.heat_color(2.0), theme.heat_color(1.0));
        let midpoint = Srgba::from(theme.heat_color(0.15));
        assert!((midpoint.red - 81.0 / 255.0).abs() < 0.000_001);
        assert!(theme.heat_emissive(0.3).red < 1.0);
        let threshold = theme.bloom().unwrap().prefilter.threshold;
        assert!(theme.heat_emissive(0.29).red < threshold);
        assert!(theme.heat_emissive(0.31).red > threshold);
        assert!(theme.heat_emissive(0.9).red > 1.0);
    }

    #[test]
    fn ink_themes_disable_heat_brightening_and_bloom() {
        for name in [
            ThemeName::Blueprint,
            ThemeName::Atlas,
            ThemeName::BevelledDesktop,
        ] {
            let theme = name.build();
            assert_eq!(theme.heat_color(0.0), theme.heat_color(1.0));
            assert!(theme.bloom().is_none());
            assert_eq!(theme.heat_emissive(1.0), LinearRgba::BLACK);
        }
    }

    #[test]
    fn candidates_have_distinct_valid_art_and_textured_grounds() {
        let mut glyphs = std::collections::HashSet::new();
        let mut grounds = std::collections::HashSet::new();
        for name in ThemeName::ALL.into_iter().skip(2) {
            let theme = name.build();
            assert!(theme.textured_ground);
            let glyph = theme.glyph_svg(theme.glyphs[0].svg, 0);
            let ground = theme.ground_svg();
            for svg in [&glyph, &ground] {
                assert!(
                    resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())
                        .is_ok()
                );
            }
            assert!(glyphs.insert(glyph));
            assert!(grounds.insert(ground));
        }
        let desktop = ThemeName::BevelledDesktop.build();
        assert_ne!(desktop.glyph_svg("", 1), desktop.glyph_svg("", 2));
    }

    #[test]
    fn heat_brightens_both_themes_without_fading_cold_files() {
        for name in [ThemeName::Neon, ThemeName::Phosphor] {
            let theme = name.build();
            let cold = LinearRgba::from(theme.heat_color(0.0));
            let warm = LinearRgba::from(theme.heat_color(0.5));
            let hot = LinearRgba::from(theme.heat_color(1.0));
            assert_eq!(cold, LinearRgba::from(theme.glyph_material.base_color));
            for (cold, warm, hot) in [
                (cold.red, warm.red, hot.red),
                (cold.green, warm.green, hot.green),
                (cold.blue, warm.blue, hot.blue),
            ] {
                assert!(hot > warm && warm > cold);
            }
            assert_eq!(cold.alpha, warm.alpha);
            assert_eq!(cold.alpha, hot.alpha);
        }
    }

    #[test]
    fn directions_use_explicit_registry_indices() {
        for (index, previous, next) in [(0, 2, 1), (1, 0, 2), (2, 1, 0)] {
            assert_eq!(ThemeDirection::Previous.index(index, 3), previous);
            assert_eq!(ThemeDirection::Next.index(index, 3), next);
        }
    }

    #[test]
    fn phosphor_contributors_have_distinct_shades() {
        let theme = ThemeName::Phosphor.build();
        let lime = Hsla::from(theme.avatars[0].base_color);
        let mint = Hsla::from(theme.avatars[2].base_color);
        assert!(mint.lightness - lime.lightness >= 0.20);
        for (index, avatar) in theme.avatars.iter().enumerate() {
            for other in &theme.avatars[index + 1..] {
                assert_ne!(avatar.base_color, other.base_color);
            }
            assert_eq!(
                theme.glass[index].base_color,
                avatar.base_color.with_alpha(0.24)
            );
        }
    }

    #[test]
    fn registry_cycles_and_keeps_one_geometry() {
        let geometry = Theme::neon().geometry();
        for name in ThemeName::ALL {
            assert_eq!(name.build().geometry(), geometry);
            assert_eq!(
                name.cycle(ThemeDirection::Next)
                    .cycle(ThemeDirection::Previous),
                name
            );
            assert_ne!(name.cycle(ThemeDirection::Next), name);
        }
        assert_eq!(ThemeName::default(), ThemeName::Neon);
        let phosphor = ThemeName::Phosphor.build();
        assert_eq!(phosphor.clear, Color::BLACK);
        assert_eq!(phosphor.floor.base_color, Color::BLACK);
        assert!(phosphor.floor.unlit);
        assert_eq!(phosphor.glyph_ink, Some([57, 255, 106]));
    }

    #[test]
    fn file_bounds_follow_icons_rings_and_optional_tiles() {
        let mut theme = Theme::neon();
        let bare = theme.geometry();
        assert!((bare.file_min - Vec3::new(-0.27, 0.015, -0.27)).length() < 0.000_001);
        assert!((bare.file_max - Vec3::new(0.27, 0.105, 0.27)).length() < 0.000_001);
        theme.file_tile = Some(FileTile {
            material: StandardMaterial::default(),
            transform: Transform::from_xyz(0.0, 0.4, 0.0)
                .with_scale(Vec3::new(0.8, 0.2, 0.8)),
        });
        let tiled = theme.geometry();
        assert!((tiled.file_min - Vec3::new(-0.4, 0.015, -0.4)).length() < 0.000_001);
        assert!((tiled.file_max - Vec3::new(0.4, 0.5, 0.4)).length() < 0.000_001);
        theme
            .file_tile
            .as_mut()
            .expect("tile style")
            .transform
            .rotation = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let rotated = theme.geometry();
        assert!((rotated.file_min - Vec3::new(-0.27, 0.0, -0.4)).length() < 0.000_001);
        assert!((rotated.file_max - Vec3::new(0.27, 0.8, 0.4)).length() < 0.000_001);
    }

    #[test]
    fn filenames_override_extensions_and_unknowns_keep_the_fallback() {
        let theme = Theme::neon();
        for (name, equivalent) in [
            ("Cargo.toml", "settings.toml"),
            ("Cargo.lock", "deps.lock"),
            ("README.MD", "readme"),
            ("Dockerfile", "image.dockerfile"),
            ("Containerfile", "Dockerfile"),
            (".gitignore", "change.patch"),
            (".bashrc", "build.sh"),
            ("main.PY", "main.py"),
            ("workflow.YML", "workflow.yaml"),
            ("index.docker-compose.yml", "Dockerfile"),
            ("file.h.in", "main.cpp"),
            ("pyproject.toml", "py.typed"),
            ("PNPM-LOCK.YAML", "pnpm-workspace.yaml"),
        ] {
            assert_ne!(theme.glyph_kind(name), 0, "{name}");
            assert_eq!(
                theme.glyph_kind(name),
                theme.glyph_kind(equivalent),
                "{name}"
            );
        }
        for name in ["unknown.xyz", "no_extension", "", ".unknown"] {
            assert_eq!(theme.glyph_kind(name), 0, "{name}");
        }
    }

    #[test]
    fn mapping_keys_are_unique_and_required_languages_have_distinct_art() {
        let theme = Theme::neon();
        let mut filenames = std::collections::HashSet::new();
        let mut extensions = std::collections::HashSet::new();
        for glyph in theme.glyphs {
            for name in &glyph.filenames {
                assert!(filenames.insert(name), "duplicate filename {name}");
            }
            for extension in &glyph.extensions {
                assert!(
                    extensions.insert(extension),
                    "duplicate extension {extension}"
                );
            }
        }
        let kinds = ["main.rs", "main.py", "Cargo.toml", "ci.yaml", "guide.md"]
            .map(|name| theme.glyph_kind(name));
        assert_eq!(
            kinds
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            5
        );
        let art: std::collections::HashSet<_> = kinds
            .into_iter()
            .map(|kind| theme.glyphs[kind].svg)
            .collect();
        assert_eq!(art.len(), 5);
        assert!(!kinds.contains(&0));
    }

    #[test]
    fn neon_has_bare_untinted_icons_and_wider_trunks() {
        let theme = Theme::neon();
        assert!(theme.file_tile.is_none());
        assert_eq!(theme.glyph_material.base_color, Color::WHITE);
        assert!(theme.glyph_material.unlit);
        assert!(theme.directory_edge_width(1) > theme.directory_edge_width(4));
        assert_eq!(
            theme.directory_edge_width(4),
            theme.directory_edge_width(u16::MAX)
        );
    }
}
