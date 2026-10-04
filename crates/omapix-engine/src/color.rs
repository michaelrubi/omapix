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
    /// A monitor's colours as its EDID gives them (`name` is the monitor's,
    /// for the description): the red, green, blue and white it shows, as a
    /// matrix profile. `None` if the EDID has none, or they aren't
    /// believable as a monitor's (many report zeros or leftovers).
    ///
    /// Its tone curve is sRGB's when the EDID says gamma 2.2 (or nothing),
    /// which is a nominal figure: greys then come out as they would with no
    /// profile, and only colours change.
    pub fn from_edid(edid: &[u8], name: &str) -> Option<Self> {
        const HEADER: [u8; 8] = [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0];
        if edid.len() < 128 || edid[..8] != HEADER {
            return None;
        }
        // Ten bits each: the low two packed into bytes 25 and 26.
        let xy = |i: usize| {
            let low = u16::from(edid[25 + i / 4] >> (6 - 2 * (i % 4))) & 3;
            f64::from(u16::from(edid[27 + i]) << 2 | low) / 1024.0
        };
        let [red, green, blue, white] = [0, 2, 4, 6].map(|i| (xy(i), xy(i + 1)));
        let area = ((green.0 - red.0) * (blue.1 - red.1) - (blue.0 - red.0) * (green.1 - red.1)).abs() / 2.0;
        let believable = red.0 > 0.55
            && red.1 < 0.4
            && green.1 > 0.5
            && blue.0 < 0.25
            && blue.1 < 0.2
            && (0.25..0.4).contains(&white.0)
            && (0.25..0.42).contains(&white.1)
            && [red, green, blue].iter().all(|c| c.1 > 0.0 && c.0 + c.1 <= 1.0)
            // sRGB's triangle is 0.112; Rec. 2020's 0.212.
            && (0.07..0.25).contains(&area);
        if !believable {
            return None;
        }
        let gamma = match edid[23] {
            0xff => 2.2,
            g => (f64::from(g) + 100.0) / 100.0,
        };
        let curve = if (gamma - 2.2).abs() < 0.05 {
            ToneCurve::new_parametric(4, &[2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045]).ok()?
        } else {
            ToneCurve::new(gamma)
        };
        let point = |(x, y): (f64, f64)| CIExyY { x, y, Y: 1.0 };
        let primaries = CIExyYTRIPLE {
            Red: point(red),
            Green: point(green),
            Blue: point(blue),
        };
        let mut profile = Profile::new_rgb(&point(white), &primaries, &[&curve, &curve, &curve]).ok()?;
        let description = format!("{name} (EDID)");
        let mut mlu = MLU::new(1);
        mlu.set_text(&description, Locale::none());
        profile.write_tag(TagSignature::ProfileDescriptionTag, Tag::MLU(&mlu));
        Some(Self {
            icc: Some(profile.icc().ok()?),
            description,
        })
    }

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

/// Soft proofing: showing on screen how an image will come out in another
/// colour space, such as a printer's for a paper, or sRGB for the web.
#[derive(Clone, Copy)]
pub struct Proof<'a> {
    pub profile: &'a ColorProfile,
    /// Show the colours as they'd come out there (Photoshop's Proof
    /// Colors): converted to it, relative colorimetric with black point
    /// compensation, and from there to the display.
    pub colors: bool,
    /// Show the colours it can't reproduce as mid grey (Gamut Warning).
    pub gamut_warning: bool,
}

/// Converts document pixels to 8 bits in a display's colours (sRGB, or a
/// monitor's profile). Safe to share between threads, so display tiles can
/// be converted in parallel.
pub struct DisplayTransform {
    transform: Shown,
    /// Proofing colours: to the proof's colour space in 16 bits (which is
    /// what leaves out the colours it hasn't got), and from there to the
    /// display. Little CMS's own soft proofing doesn't clip to a matrix
    /// profile such as sRGB.
    proof: Option<(Transform<Pixel, Pixel, GlobalContext, DisallowCache>, Shown)>,
    /// The gamut warning: a transform that gives the colours the proof
    /// hasn't got as Little CMS's [`ALARM`] colour. (What it gives the
    /// others isn't used: it proofs them its own way.)
    warning: Option<Shown>,
}

type Shown = Transform<Pixel, [u8; 4], GlobalContext, DisallowCache>;

/// Little CMS's alarm colour as it's set to begin with (0x7F00 a channel),
/// in 8 bits: Photoshop's gamut warning is a mid grey too.
const ALARM: [u8; 3] = [127; 3];

impl DisplayTransform {
    pub fn to_srgb(source: &ColorProfile) -> Result<Self> {
        Self::new(source, &ColorProfile::srgb(), None)
    }

    pub fn new(source: &ColorProfile, display: &ColorProfile, proof: Option<Proof>) -> Result<Self> {
        let (source, display) = (source.lcms_profile()?, display.lcms_profile()?);
        let (from, to) = (PixelFormat::RGBA_16, PixelFormat::RGBA_8);
        let intent = Intent::RelativeColorimetric;
        let flags = Flags::NO_CACHE | Flags::BLACKPOINT_COMPENSATION;
        let context = GlobalContext::new;
        let transform = Transform::new_flags_context(context(), &source, from, &display, to, intent, flags)?;
        let (mut proofed, mut warning) = (None, None);
        if let Some(proof) = proof {
            let profile = proof.profile.lcms_profile()?;
            // Four 16-bit values either way.
            let between = match profile.color_space() {
                lcms2::ColorSpaceSignature::RgbData => PixelFormat::RGBA_16,
                lcms2::ColorSpaceSignature::CmykData => PixelFormat::CMYK_16,
                _ => {
                    let name = proof.profile.description();
                    return Err(Error::Unsupported(format!("{name} is neither an RGB nor a CMYK profile")));
                }
            };
            if proof.colors {
                proofed = Some((
                    Transform::new_flags_context(context(), &source, from, &profile, between, intent, flags)?,
                    Transform::new_flags_context(context(), &profile, between, &display, to, intent, flags)?,
                ));
            }
            if proof.gamut_warning {
                let flags = flags | Flags::GAMUT_CHECK;
                warning = Some(Transform::new_proofing_context(
                    context(),
                    &source,
                    from,
                    &display,
                    to,
                    &profile,
                    intent,
                    intent,
                    flags,
                )?);
            }
        }
        Ok(Self {
            transform,
            proof: proofed,
            warning,
        })
    }

    /// Convert `src` into `dst`. Alpha is narrowed to 8 bits rather than
    /// colour managed.
    pub fn convert(&self, src: &[Pixel], dst: &mut [[u8; 4]]) {
        match &self.proof {
            None => self.transform.transform_pixels(src, dst),
            Some((to_proof, shown)) => {
                let mut between = vec![[0u16; 4]; src.len()];
                to_proof.transform_pixels(src, &mut between);
                shown.transform_pixels(&between, dst);
            }
        }
        if let Some(warning) = &self.warning {
            let mut warned = vec![[0u8; 4]; src.len()];
            warning.transform_pixels(src, &mut warned);
            for (out, warned) in dst.iter_mut().zip(&warned) {
                if warned[..3] == ALARM {
                    out[..3].copy_from_slice(&ALARM);
                }
            }
        }
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

    /// An EDID with these colours (x, y of red, green, blue and white) and
    /// gamma byte.
    pub(crate) fn edid(colours: [(f64, f64); 4], gamma: u8) -> Vec<u8> {
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]);
        edid[23] = gamma;
        for (i, v) in colours.iter().flat_map(|&(x, y)| [x, y]).enumerate() {
            let bits = (v * 1024.0).round() as u16;
            edid[27 + i] = (bits >> 2) as u8;
            edid[25 + i / 4] |= ((bits & 3) as u8) << (6 - 2 * (i % 4));
        }
        edid
    }

    /// A wide-gamut laptop OLED's colours.
    pub(crate) const OLED: [(f64, f64); 4] = [(0.680, 0.320), (0.237, 0.723), (0.140, 0.050), (0.3125, 0.329)];

    #[test]
    fn a_monitors_edid_colours_make_its_profile() {
        let monitor = ColorProfile::from_edid(&edid(OLED, 120), "eDP-2").unwrap();
        assert_eq!(monitor.description(), "eDP-2 (EDID)");
        let t = DisplayTransform::new(&ColorProfile::srgb(), &monitor, None).unwrap();
        let src = [
            [65535, 0, 0, 65535],
            [65535, 65535, 65535, 65535],
            [32896, 32896, 32896, 65535],
            [0, 0, 0, 65535],
        ];
        let mut dst = [[0u8; 4]; 4];
        t.convert(&src, &mut dst);
        // sRGB's red is well inside the monitor's: less of its red, and some
        // green and blue to pull it in.
        let [r, g, b, _] = dst[0];
        assert!(r < 245 && g > 40 && b > 10, "{:?}", dst[0]);
        // Greys stay as they are, the EDID's gamma 2.2 being taken as sRGB's.
        assert_eq!(dst[1], [255; 4]);
        assert_eq!(dst[3], [0, 0, 0, 255]);
        assert!(dst[2][..3].iter().all(|&v| v.abs_diff(128) <= 1), "{:?}", dst[2]);

        // Another gamma is taken at its word: 1.8 shows mid grey lighter, so
        // it's sent darker.
        let mac = ColorProfile::from_edid(&edid(OLED, 80), "old").unwrap();
        let t = DisplayTransform::new(&ColorProfile::srgb(), &mac, None).unwrap();
        t.convert(&src, &mut dst);
        assert!(dst[2][0] < 120, "{:?}", dst[2]);
    }

    #[test]
    fn proofing_shows_what_another_colour_space_makes_of_the_colours() {
        // A ProPhoto image proofed for the web, on a wide-gamut monitor: a
        // green outside sRGB, one inside it, and a grey.
        let source = linear_prophoto().with_gamma(1.8).unwrap();
        let monitor = ColorProfile::from_edid(&edid(OLED, 120), "eDP-2").unwrap();
        let srgb = ColorProfile::srgb();
        let src = [[10000, 60000, 10000, 65535], [30000, 36000, 30000, 65535], [32000, 32000, 32000, 40000]];
        let shown = |colors, gamut_warning| {
            let proof = Proof {
                profile: &srgb,
                colors,
                gamut_warning,
            };
            let mut dst = [[0u8; 4]; 3];
            DisplayTransform::new(&source, &monitor, Some(proof)).unwrap().convert(&src, &mut dst);
            dst
        };
        let plain = shown(false, false);
        let mut unproofed = [[0u8; 4]; 3];
        DisplayTransform::new(&source, &monitor, None).unwrap().convert(&src, &mut unproofed);
        assert_eq!(plain, unproofed);

        // Proofed, the vivid green is pulled in to sRGB's (less of the
        // monitor's green, more of its red); the others hardly move.
        let proofed = shown(true, false);
        assert!(proofed[0][1] < plain[0][1] || proofed[0][0] > plain[0][0] + 10, "{proofed:?} vs {plain:?}");
        for i in [1, 2] {
            for c in 0..3 {
                assert!(proofed[i][c].abs_diff(plain[i][c]) <= 2, "{proofed:?} vs {plain:?}");
            }
        }
        // The gamut warning greys it, with or without the proof, and
        // leaves the others and every alpha alone.
        for warned in [shown(false, true), shown(true, true)] {
            let [r, g, b, a] = warned[0];
            assert_eq!([r, g, b], ALARM, "{warned:?}");
            assert_eq!((a, warned[2][3]), (255, 40000u16.to_be_bytes()[0]));
            for c in 0..3 {
                assert!(warned[1][c].abs_diff(plain[1][c]) <= 2, "{warned:?} vs {plain:?}");
            }
        }
    }

    /// `cargo test -p omapix-engine printer -- --ignored`, with Krita's and
    /// Ghostscript's profiles installed.
    #[test]
    #[ignore]
    fn proofing_for_a_printer_profile() {
        let read = |path: &str| ColorProfile::from_icc(std::fs::read(path).unwrap()).unwrap();
        let (srgb, cmyk) = (ColorProfile::srgb(), read("/usr/share/color/icc/krita/cmyk.icm"));
        // sRGB's blue is far outside what the inks print; a soft brown isn't.
        let src = [[0, 0, 65535, 65535], [40000, 30000, 25000, 65535]];
        let shown = |colors, gamut_warning| {
            let proof = Proof {
                profile: &cmyk,
                colors,
                gamut_warning,
            };
            let mut dst = [[0u8; 4]; 2];
            DisplayTransform::new(&srgb, &srgb, Some(proof)).unwrap().convert(&src, &mut dst);
            dst
        };
        let (plain, proofed, warned) = (shown(false, false), shown(true, false), shown(true, true));
        assert_eq!(plain[0], [0, 0, 255, 255]);
        assert!(proofed[0][0] > 20 && proofed[0][2] < 235, "{proofed:?}");
        assert!((0..3).all(|c| proofed[1][c].abs_diff(plain[1][c]) < 12), "{proofed:?} vs {plain:?}");
        assert_eq!(warned[0][..3], ALARM, "{warned:?}");
        assert_eq!(warned[1], proofed[1]);
        let grey = read("/usr/share/ghostscript/iccprofiles/sgray.icc");
        let proof = Proof {
            profile: &grey,
            colors: true,
            gamut_warning: false,
        };
        assert!(DisplayTransform::new(&srgb, &srgb, Some(proof)).is_err());
    }

    #[test]
    fn an_edid_without_believable_colours_gives_no_profile() {
        assert!(ColorProfile::from_edid(&edid([(0.0, 0.0); 4], 120), "m").is_none(), "zeros");
        assert!(ColorProfile::from_edid(&edid(OLED, 120)[..100], "m").is_none(), "cut short");
        let mut no_header = edid(OLED, 120);
        no_header[0] = 1;
        assert!(ColorProfile::from_edid(&no_header, "m").is_none());
        // Red and green swapped, a white that's far too blue, a sliver.
        let [r, g, b, w] = OLED;
        for bad in [[g, r, b, w], [r, g, b, (0.2, 0.2)], [r, (0.6, 0.38), b, w]] {
            assert!(ColorProfile::from_edid(&edid(bad, 120), "m").is_none(), "{bad:?}");
        }
        // sRGB's own, as most desktop monitors report.
        let srgb = [(0.64, 0.33), (0.30, 0.60), (0.15, 0.06), (0.3127, 0.329)];
        assert!(ColorProfile::from_edid(&edid(srgb, 0xff), "m").is_some());
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
