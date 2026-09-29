use lcms2::{
    CIExyY, CIExyYTRIPLE, DisallowCache, Flags, GlobalContext, InfoType, Intent, Locale, MLU,
    PixelFormat, Profile, Tag, TagSignature, ToneCurve, Transform,
};

use crate::tiled::Tiled;
use crate::{Error, Pixel, Result};

/// The ICC reference white (D50), which matrix profiles' colorants are
/// relative to.
const D50: CIExyY = CIExyY {
    x: 0.3457,
    y: 0.3585,
    Y: 1.0,
};

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

    /// True for RGB profiles with linear tone curves (gamma 1.0), like
    /// darktable's default "Linear ProPhoto RGB" export.
    pub fn is_linear(&self) -> bool {
        let Ok(profile) = self.lcms_profile() else {
            return false;
        };
        [
            TagSignature::RedTRCTag,
            TagSignature::GreenTRCTag,
            TagSignature::BlueTRCTag,
        ]
        .into_iter()
        .all(|sig| match profile.read_tag(sig) {
            Tag::ToneCurve(curve) => curve
                .estimated_gamma(0.01)
                .is_some_and(|g| (g - 1.0).abs() < 0.02),
            _ => false,
        })
    }

    /// The same colour space (primaries and white) with a gamma-encoded tone
    /// curve. Only works for matrix/TRC RGB profiles.
    pub fn with_gamma(&self, gamma: f64) -> Result<Self> {
        let profile = self.lcms_profile()?;
        let colorant = |sig| match profile.read_tag(sig) {
            Tag::CIEXYZ(xyz) => Some(lcms2::XYZ2xyY(xyz)),
            _ => None,
        };
        let (Some(red), Some(green), Some(blue)) = (
            colorant(TagSignature::RedColorantTag),
            colorant(TagSignature::GreenColorantTag),
            colorant(TagSignature::BlueColorantTag),
        ) else {
            return Err(Error::Unsupported(format!(
                "{} is not a matrix RGB profile",
                self.description
            )));
        };
        let curve = ToneCurve::new(gamma);
        let primaries = CIExyYTRIPLE {
            Red: red,
            Green: green,
            Blue: blue,
        };
        let mut out = Profile::new_rgb(&D50, &primaries, &[&curve, &curve, &curve])?;
        let base = self
            .description
            .trim_start_matches("Linear ")
            .trim_start_matches("linear ");
        let description = format!("{base} (gamma {gamma})");
        let mut mlu = MLU::new(1);
        mlu.set_text(&description, Locale::none());
        out.write_tag(TagSignature::ProfileDescriptionTag, Tag::MLU(&mlu));
        Ok(Self {
            icc: Some(out.icc()?),
            description,
        })
    }
}

impl ColorProfile {
    /// An 8-bit sRGB colour (from a colour picker) in this colour space.
    pub fn from_srgb8(&self, rgb: [u8; 3]) -> Result<Pixel> {
        let transform: Transform<[u8; 3], [u16; 3]> = Transform::new(
            &Profile::new_srgb(),
            PixelFormat::RGB_8,
            &self.lcms_profile()?,
            PixelFormat::RGB_16,
            Intent::RelativeColorimetric,
        )?;
        let mut out = [[0u16; 3]];
        transform.transform_pixels(&[rgb], &mut out);
        let [r, g, b] = out[0];
        Ok([r, g, b, u16::MAX])
    }
}

/// Convert an image between colour spaces, keeping alpha as it is.
pub fn convert(
    image: &Tiled<Pixel>,
    from: &ColorProfile,
    to: &ColorProfile,
) -> Result<Tiled<Pixel>> {
    let transform: Transform<Pixel, Pixel, GlobalContext, DisallowCache> =
        Transform::new_flags_context(
            GlobalContext::new(),
            &from.lcms_profile()?,
            PixelFormat::RGBA_16,
            &to.lcms_profile()?,
            PixelFormat::RGBA_16,
            Intent::RelativeColorimetric,
            Flags::NO_CACHE | Flags::BLACKPOINT_COMPENSATION,
        )?;
    let mut out = image.clone();
    out.par_update(|_, _, tile| {
        let tile = tile?;
        let mut converted = vec![[0u16; 4]; tile.len()];
        transform.transform_pixels(tile, &mut converted);
        for (dst, src) in converted.iter_mut().zip(tile) {
            dst[3] = src[3];
        }
        Some(converted)
    });
    Ok(out)
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

/// Converts document pixels to 16-bit linear-light sRGB, dropping alpha,
/// for the JPEG encoder: it rounds to 8 bits only after its own maths, so
/// smooth gradients don't band.
pub struct LinearSrgbTransform {
    transform: Transform<Pixel, [u16; 3], GlobalContext, DisallowCache>,
}

impl LinearSrgbTransform {
    pub fn new(source: &ColorProfile) -> Result<Self> {
        let transform = Transform::new_flags_context(
            GlobalContext::new(),
            &source.lcms_profile()?,
            PixelFormat::RGBA_16,
            &ColorProfile::srgb().with_gamma(1.0)?.lcms_profile()?,
            PixelFormat::RGB_16,
            Intent::RelativeColorimetric,
            Flags::NO_CACHE | Flags::BLACKPOINT_COMPENSATION,
        )?;
        Ok(Self { transform })
    }

    pub fn convert(&self, src: &[Pixel], dst: &mut [[u16; 3]]) {
        self.transform.transform_pixels(src, dst);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn linear_prophoto() -> ColorProfile {
        let curve = ToneCurve::new(1.0);
        let primaries = CIExyYTRIPLE {
            Red: CIExyY {
                x: 0.7347,
                y: 0.2653,
                Y: 1.0,
            },
            Green: CIExyY {
                x: 0.1596,
                y: 0.8404,
                Y: 1.0,
            },
            Blue: CIExyY {
                x: 0.0366,
                y: 0.0001,
                Y: 1.0,
            },
        };
        let mut p = Profile::new_rgb(&D50, &primaries, &[&curve, &curve, &curve]).unwrap();
        let mut mlu = MLU::new(1);
        mlu.set_text("Linear ProPhoto RGB", Locale::none());
        p.write_tag(TagSignature::ProfileDescriptionTag, Tag::MLU(&mlu));
        ColorProfile::from_icc(p.icc().unwrap()).unwrap()
    }

    #[test]
    fn srgb_black_and_white_map_to_extremes() {
        let gamma = linear_prophoto().with_gamma(1.8).unwrap();
        assert_eq!(gamma.from_srgb8([0, 0, 0]).unwrap(), [0, 0, 0, 65535]);
        let white = gamma.from_srgb8([255, 255, 255]).unwrap();
        assert!(white[..3].iter().all(|&v| v > 65400), "{white:?}");
    }

    #[test]
    fn detects_linear_profiles() {
        assert!(linear_prophoto().is_linear());
        assert!(!ColorProfile::srgb().is_linear());
    }

    #[test]
    fn gamma_version_keeps_colours_and_lifts_shadows() {
        let linear = linear_prophoto();
        let gamma = linear.with_gamma(1.8).unwrap();
        assert_eq!(gamma.description(), "ProPhoto RGB (gamma 1.8)");
        assert!(!gamma.is_linear());

        // 18 % grey plus a saturated colour, with partial alpha.
        let px = vec![[11796u16, 11796, 11796, 65535], [40000, 9000, 3000, 30000]];
        let image = Tiled::from_slice(2, 1, [0; 4], &px);
        let converted = convert(&image, &linear, &gamma).unwrap();
        let grey = converted.get(0, 0);
        let expected = (0.18f64.powf(1.0 / 1.8) * 65535.0) as u16;
        assert!(grey[0].abs_diff(expected) < 200, "{grey:?} vs {expected}");
        assert_eq!(converted.get(1, 0)[3], 30000);

        // Converting back returns the original values, within Little CMS's
        // 16-bit precision (a few parts in 65535).
        let back = convert(&converted, &gamma, &linear).unwrap();
        for (a, b) in back.to_vec().iter().zip(&px) {
            for c in 0..3 {
                assert!(a[c].abs_diff(b[c]) <= 20, "{a:?} vs {b:?}");
            }
        }
    }

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
