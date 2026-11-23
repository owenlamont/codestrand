use std::fmt::Write;

use bevy::color::Srgba;

use super::ThemeName;

impl ThemeName {
    pub(crate) fn tile_floor_art(self) -> Option<String> {
        let art = match self {
            Self::Neon | Self::Phosphor => return None,
            Self::Outrun => {
                r##"<defs><linearGradient id="d" x2="0" y2="1"><stop stop-color="#12022a"/><stop offset="1" stop-color="#3b0a45"/></linearGradient></defs><rect width="1024" height="1024" fill="url(#d)"/>"##
            }
            Self::Blueprint => {
                r##"<defs><linearGradient id="d" x2="0" y2="1"><stop stop-color="#0d3a73"/><stop offset="1" stop-color="#0f4486"/></linearGradient></defs><rect width="1024" height="1024" fill="url(#d)"/>"##
            }
            Self::RetroDesktop => {
                r##"<rect width="1024" height="1024" fill="#0f7b7b"/><defs><pattern id="p" width="8" height="8" patternUnits="userSpaceOnUse"><path d="M0 0h1v1H0zM4 4h1v1H4z" fill="#146e70"/></pattern></defs><rect width="1024" height="1024" fill="url(#p)"/>"##
            }
            Self::Atlas => return Some(atlas_tile_floor()),
            Self::Forge => {
                r##"<defs><radialGradient id="d"><stop stop-color="#241109"/><stop offset=".6" stop-color="#140a06"/><stop offset="1" stop-color="#070505"/></radialGradient></defs><rect width="1024" height="1024" fill="url(#d)"/>"##
            }
        };
        Some(art.to_owned())
    }

    pub(crate) fn ground(self) -> (u32, &'static str) {
        match self {
            Self::Neon => (
                0x05_0910,
                r##"<path d="M0 32H256M0 96H256M0 160H256M0 224H256M32 0V256M96 0V256M160 0V256M224 0V256" stroke="#1b384c" stroke-width="1.4"/>"##,
            ),
            Self::Phosphor => (
                0x03_0905,
                r##"<path d="M0 32H256M0 96H256M0 160H256M0 224H256M32 0V256M96 0V256M160 0V256M224 0V256" stroke="#1b4226" stroke-width="1.4"/>"##,
            ),
            Self::Outrun => (
                0x19_082e,
                r##"<path d="M0 32H256M0 96H256M0 160H256M0 224H256M32 0V256M96 0V256M160 0V256M224 0V256" stroke="#69254f" stroke-width="1.6"/>"##,
            ),
            Self::Blueprint => (
                0x0e_3d77,
                r##"<defs><pattern id="grid" width="16" height="16" patternUnits="userSpaceOnUse"><path d="M0 8H16M8 0V16" stroke="#194a83"/></pattern></defs><rect width="256" height="256" fill="url(#grid)"/><path d="M0 8H256M0 72H256M0 136H256M0 200H256M8 0V256M72 0V256M136 0V256M200 0V256" stroke="#27598e"/>"##,
            ),
            Self::RetroDesktop => (
                0x0f_7b7b,
                r##"<defs><pattern id="dither" width="8" height="8" patternUnits="userSpaceOnUse"><path d="M0 0h1v1H0zM4 4h1v1H4z" fill="#146e70"/></pattern></defs><rect width="256" height="256" fill="url(#dither)"/>"##,
            ),
            Self::Atlas => (
                0xe8_ebe2,
                r##"<defs><pattern id="contours" width="256" height="64" patternUnits="userSpaceOnUse"><path d="M0 24C64 24 64 8 128 24S192 24 256 24M0 50C64 50 64 34 128 50S192 50 256 50" fill="none" stroke="#8e9c83" stroke-opacity=".27"/></pattern></defs><rect width="256" height="256" fill="url(#contours)"/>"##,
            ),
            Self::Forge => (
                0x1a_100d,
                r##"<path d="M24 40h72m48 28h46M12 142h38m28 28h90m30 42h42M48 230h54" fill="none" stroke="#261814"/>"##,
            ),
        }
    }

    pub(crate) fn sky(self) -> (Srgba, Srgba) {
        let (horizon, zenith) = match self {
            Self::Neon => ("#243b50", "#101b30"),
            Self::Phosphor => ("#214331", "#10261c"),
            Self::Outrun => ("#70375b", "#25143d"),
            Self::Blueprint => ("#081b33", "#102846"),
            Self::RetroDesktop => ("#a0c5c7", "#658b9b"),
            Self::Atlas => ("#a6b6b4", "#839caa"),
            Self::Forge => ("#714438", "#32211f"),
        };
        (
            Srgba::hex(horizon).expect("sky horizon colour"),
            Srgba::hex(zenith).expect("sky zenith colour"),
        )
    }
}

fn atlas_tile_floor() -> String {
    let mut art = String::from(
        r##"<defs><linearGradient id="d" x2="0" y2="1"><stop stop-color="#eceee6"/><stop offset="1" stop-color="#e3e6dc"/></linearGradient></defs><rect width="1024" height="1024" fill="url(#d)"/>"##,
    );
    for i in -10..35 {
        let y = i * 38;
        write!(art, r##"<path d="M-100 {y}q180 -85 350 15t400 -30t500 10" fill="none" stroke="#8e9c83" stroke-width="1" stroke-opacity=".18"/>"##).expect("string write");
    }
    for x in (0..1024).step_by(17) {
        write!(art, r##"<path d="M{x} 0l-400 1024" fill="none" stroke="#8e9c83" stroke-width="1" stroke-opacity=".035"/>"##).expect("string write");
    }
    art
}
