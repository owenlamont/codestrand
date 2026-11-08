use std::fmt::Write;

use super::{Theme, ThemeName};

impl Theme {
    pub(crate) fn backdrop_svg(&self) -> Option<&'static str> {
        (self.name == ThemeName::Outrun).then_some(r##"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024"><defs><linearGradient id="s" x2="0" y2="1"><stop stop-color="#ffd36b"/><stop offset="1" stop-color="#ff3f8e"/></linearGradient><mask id="b"><circle cx="512" cy="512" r="450" fill="white"/><path d="M0 560H1024M0 620H1024M0 680H1024M0 740H1024M0 800H1024M0 860H1024M0 920H1024" stroke="black" stroke-width="24"/></mask></defs><rect width="1024" height="1024" fill="url(#s)" mask="url(#b)"/></svg>"##)
    }

    pub(crate) fn ground_svg(&self) -> String {
        let mut art = String::new();
        match self.name {
            ThemeName::Outrun => {
                art.push_str(r##"<defs><linearGradient id="d" x2="0" y2="1"><stop stop-color="#12022a"/><stop offset="1" stop-color="#3b0a45"/></linearGradient></defs><rect width="1024" height="1024" fill="url(#d)"/>"##);
                for x in (-1024..=2048).step_by(128) {
                    write!(art, r##"<path d="M512 330L{x} 1024" stroke="#ff3fa4" stroke-opacity=".32"/>"##).expect("string write");
                }
                for y in [340, 360, 390, 435, 500, 590, 710, 865, 1020] {
                    write!(art, r##"<path d="M0 {y}H1024" stroke="#ff3fa4" stroke-opacity=".32"/>"##).expect("string write");
                }
            }
            ThemeName::Blueprint => {
                art.push_str(r##"<defs><linearGradient id="d" x2="0" y2="1"><stop stop-color="#0d3a73"/><stop offset="1" stop-color="#0f4486"/></linearGradient><pattern id="g" width="20" height="20" patternUnits="userSpaceOnUse"><path d="M20 0H0V20" fill="none" stroke="white" stroke-opacity=".1"/></pattern><pattern id="b" width="100" height="100" patternUnits="userSpaceOnUse"><path d="M100 0H0V100" fill="none" stroke="white" stroke-opacity=".22"/></pattern></defs><rect width="1024" height="1024" fill="url(#d)"/><rect width="1024" height="1024" fill="url(#g)"/><rect width="1024" height="1024" fill="url(#b)"/><rect x="20" y="20" width="984" height="984" fill="none" stroke="white" stroke-opacity=".6" stroke-width="2"/>"##);
            }
            ThemeName::BevelledDesktop => {
                art.push_str(r##"<rect width="1024" height="1024" fill="#0f7b7b"/><defs><pattern id="p" width="8" height="8" patternUnits="userSpaceOnUse"><path d="M0 0h1v1H0zM4 4h1v1H4z" fill="#146e70"/></pattern></defs><rect width="1024" height="1024" fill="url(#p)"/>"##);
            }
            ThemeName::Atlas => {
                art.push_str(r##"<defs><linearGradient id="d" x2="0" y2="1"><stop stop-color="#eceee6"/><stop offset="1" stop-color="#e3e6dc"/></linearGradient></defs><rect width="1024" height="1024" fill="url(#d)"/>"##);
                for i in -10..35 {
                    let y = i * 38;
                    write!(art, r##"<path d="M-100 {y}q180 -85 350 15t400 -30t500 10" fill="none" stroke="#8e9c83" stroke-width="1" stroke-opacity=".18"/>"##).expect("string write");
                }
                for x in (0..1024).step_by(17) {
                    write!(art, r##"<path d="M{x} 0l-400 1024" stroke="#8e9c83" stroke-opacity=".035"/>"##).expect("string write");
                }
            }
            ThemeName::Forge => {
                art.push_str(r##"<defs><radialGradient id="d"><stop stop-color="#241109"/><stop offset=".6" stop-color="#140a06"/><stop offset="1" stop-color="#070505"/></radialGradient></defs><rect width="1024" height="1024" fill="url(#d)"/>"##);
            }
            ThemeName::Neon | ThemeName::Phosphor => {
                art.push_str(r#"<rect width="1024" height="1024" fill="black"/>"#);
            }
        }
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024">{art}</svg>"#
        )
    }
}
