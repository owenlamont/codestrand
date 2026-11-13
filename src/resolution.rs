use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Resolution {
    pub width: u16,
    pub height: u16,
}

impl Default for Resolution {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
        }
    }
}

pub(crate) fn ui_scale(logical_height: f32) -> f32 {
    logical_height.max(1.0) / 720.0
}

impl fmt::Display for Resolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}x{}", self.width, self.height)
    }
}

impl FromStr for Resolution {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || {
            "resolution must be WIDTHxHEIGHT, each side 64–8192 and at most 33,554,432 pixels".to_owned()
        };
        let (width, height) = value.split_once('x').ok_or_else(invalid)?;
        let width: u16 = width.parse().map_err(|_| invalid())?;
        let height: u16 = height.parse().map_err(|_| invalid())?;
        if !(64..=8192).contains(&width)
            || !(64..=8192).contains(&height)
            || u32::from(width) * u32::from(height) > 33_554_432
        {
            return Err(invalid());
        }
        Ok(Self { width, height })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_sizes_have_720_logical_rows() {
        for value in [
            "1280x720",
            "1920x1080",
            "3840x2160",
            "2560x1600",
            "3440x1440",
            "5120x1440",
            "7680x4320",
        ] {
            let size: Resolution = value.parse().unwrap();
            assert_eq!(size.to_string(), value);
            assert!(
                (f32::from(size.height) / ui_scale(f32::from(size.height)) - 720.0)
                    .abs()
                    < 0.001
            );
        }
        assert_eq!(Resolution::default().to_string(), "1280x720");
    }

    #[test]
    fn rejects_invalid_or_unbounded_sizes() {
        for value in [
            "",
            "1920",
            "0x720",
            "63x720",
            "8193x720",
            "8192x8192",
            "65536x720",
            "1920x1080x2",
            "-1x720",
        ] {
            assert!(value.parse::<Resolution>().is_err(), "{value}");
        }
    }
}
