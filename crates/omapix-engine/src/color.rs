use lcms2::{
    DisallowCache, Flags, GlobalContext, InfoType, Intent, Locale, PixelFormat, Profile, Transform,
};

use crate::{Pixel, Result};

/// The colour space a document's pixel values are in.
#[derive(Clone)]
pub struct ColorProfile {
    /// Embedded ICC profile. `None` means the file had none and is treated as sRGB.
    icc: Option<Vec<u8>>,
    description: String,
}

impl ColorProfile {
    pub fn srgb() -> Self {
        Self {
            icc: None,
            description: "sRGB (assumed)".into(),
        }
    }

    pub fn from_icc(icc: Vec<u8>) -> Result<Self> {
        let profile = Profile::new_icc(&icc)?;
        let description = profile
            .info(InfoType::Description, Locale::none())
            .unwrap_or_else(|| "Embedded profile".into());
        Ok(Self {
            icc: Some(icc),
            description,
        })
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn icc(&self) -> Option<&[u8]> {
        self.icc.as_deref()
    }

    fn lcms_profile(&self) -> Result<Profile> {
        Ok(match &self.icc {
            Some(icc) => Profile::new_icc(icc)?,
            None => Profile::new_srgb(),
        })
    }
}

/// Converts document pixels to 8-bit sRGB for display. Safe to share
/// between threads, so display tiles can be converted in parallel.
pub struct DisplayTransform {
    transform: Transform<Pixel, [u8; 4], GlobalContext, DisallowCache>,
}

impl DisplayTransform {
    pub fn to_srgb(source: &ColorProfile) -> Result<Self> {
        let transform = Transform::new_flags_context(
            GlobalContext::new(),
            &source.lcms_profile()?,
            PixelFormat::RGBA_16,
            &Profile::new_srgb(),
            PixelFormat::RGBA_8,
            Intent::RelativeColorimetric,
            Flags::NO_CACHE | Flags::BLACKPOINT_COMPENSATION,
        )?;
        Ok(Self { transform })
    }

    /// Convert `src` into `dst`. Alpha is narrowed to 8 bits rather than
    /// colour managed.
    pub fn convert(&self, src: &[Pixel], dst: &mut [[u8; 4]]) {
        self.transform.transform_pixels(src, dst);
        for (out, px) in dst.iter_mut().zip(src) {
            out[3] = (px[3] >> 8) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_to_srgb_is_identity_within_rounding() {
        let t = DisplayTransform::to_srgb(&ColorProfile::srgb()).unwrap();
        let src = [
            [0, 0, 0, 65535],
            [65535, 65535, 65535, 65535],
            [32896, 16448, 49344, 32896],
        ];
        let mut dst = [[0u8; 4]; 3];
        t.convert(&src, &mut dst);
        assert_eq!(dst[0], [0, 0, 0, 255]);
        assert_eq!(dst[1], [255, 255, 255, 255]);
        for (got, want) in dst[2].iter().zip([128u8, 64, 192, 128]) {
            assert!(got.abs_diff(want) <= 1, "{:?}", dst[2]);
        }
    }
}
