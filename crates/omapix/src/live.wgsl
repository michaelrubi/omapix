// Live compositing on the GPU (see live.rs and gpu.rs).
//
// `layer` blends one layer onto what's below it, a pass per layer, exactly
// as `composite.rs` does on the CPU: straight alpha, 0–1 values in the
// document's colour space, Photoshop's blend formulas (`blend.rs`).
// `display` draws the result on the canvas through the display colour
// transform, baked into a 3D lookup table.

struct Layer {
    // Level pixel of the region's first texel.
    region_origin: vec2<i32>,
    // Level pixels the layer is drawn moved by.
    offset: vec2<i32>,
    plane_origin: vec2<i32>,
    plane_size: vec2<i32>,
    mask_origin: vec2<i32>,
    mask_size: vec2<i32>,
    pixel_fill: vec4<f32>,
    mode: u32,
    // 0: pixels, 1: an adjustment, 2: the first (nothing below it).
    kind: u32,
    has_mask: u32,
    lut_size: u32,
    opacity: f32,
    mask_fill: f32,
    // 1: the layer is drawn through `inverse` (Free Transform), sampled
    // between its pixels, rather than moved by `offset`.
    transformed: u32,
    pad: u32,
    // Level pixel on the layer shown at level pixel (x, y): (a·x + b·y + c,
    // d·x + e·y + f), as (a, b, c, d) and (e, f).
    inverse_abcd: vec4<f32>,
    inverse_ef: vec4<f32>,
};

@group(0) @binding(0) var below: texture_2d<f32>;
@group(0) @binding(1) var pixels: texture_2d<u32>;
@group(0) @binding(2) var mask: texture_2d<u32>;
@group(0) @binding(3) var lut: texture_3d<f32>;
@group(0) @binding(4) var<uniform> p: Layer;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering the target.
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

fn inside(q: vec2<i32>, origin: vec2<i32>, size: vec2<i32>) -> bool {
    let d = q - origin;
    return all(d >= vec2<i32>(0)) && all(d < size);
}

fn pixel_at(q: vec2<i32>) -> vec4<f32> {
    if !inside(q, p.plane_origin, p.plane_size) {
        return p.pixel_fill;
    }
    return vec4<f32>(textureLoad(pixels, q - p.plane_origin, 0)) / 65535.0;
}

fn mask_at(q: vec2<i32>) -> f32 {
    if p.has_mask == 0u {
        return 1.0;
    }
    if !inside(q, p.mask_origin, p.mask_size) {
        return p.mask_fill;
    }
    return f32(textureLoad(mask, q - p.mask_origin, 0).r) / 65535.0;
}

struct Sampled {
    // Straight alpha.
    colour: vec4<f32>,
    mask: f32,
};

// The transformed layer's pixel and mask at level pixel `q`, interpolated
// between the four nearest (premultiplied), as the CPU's bilinear preview
// does. Outside the layer it's transparent.
fn transformed_at(q: vec2<i32>) -> Sampled {
    let at = vec2<f32>(q) + 0.5;
    let s = vec2<f32>(
        dot(p.inverse_abcd.xy, at) + p.inverse_abcd.z,
        dot(vec2<f32>(p.inverse_abcd.w, p.inverse_ef.x), at) + p.inverse_ef.y,
    ) - 0.5;
    let i = vec2<i32>(floor(s));
    let f = s - floor(s);
    var sum = vec4<f32>(0.0);
    var mask = 0.0;
    for (var k = 0; k < 4; k++) {
        let t = i + vec2<i32>(k & 1, k >> 1u);
        let wx = select(1.0 - f.x, f.x, (k & 1) == 1);
        let wy = select(1.0 - f.y, f.y, (k >> 1u) == 1);
        var c = vec4<f32>(0.0);
        if inside(t, p.plane_origin, p.plane_size) {
            c = vec4<f32>(textureLoad(pixels, t - p.plane_origin, 0)) / 65535.0;
        }
        sum += vec4<f32>(c.rgb * c.a, c.a) * wx * wy;
        mask += mask_at(t) * wx * wy;
    }
    var colour = vec4<f32>(0.0);
    if sum.a > 0.0 {
        colour = vec4<f32>(sum.rgb / sum.a, sum.a);
    }
    return Sampled(colour, mask);
}

// Trilinear interpolation of a lookup table, as a 3D texture of 32-bit
// floats (which can't be filtered by the sampler everywhere).
fn lookup(table: texture_3d<f32>, size: u32, c: vec3<f32>) -> vec3<f32> {
    let n = f32(size - 1u);
    let x = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)) * n;
    let i0 = vec3<i32>(floor(x));
    let i1 = min(i0 + vec3<i32>(1), vec3<i32>(i32(size) - 1));
    let f = x - floor(x);
    let c000 = textureLoad(table, vec3<i32>(i0.x, i0.y, i0.z), 0).rgb;
    let c100 = textureLoad(table, vec3<i32>(i1.x, i0.y, i0.z), 0).rgb;
    let c010 = textureLoad(table, vec3<i32>(i0.x, i1.y, i0.z), 0).rgb;
    let c110 = textureLoad(table, vec3<i32>(i1.x, i1.y, i0.z), 0).rgb;
    let c001 = textureLoad(table, vec3<i32>(i0.x, i0.y, i1.z), 0).rgb;
    let c101 = textureLoad(table, vec3<i32>(i1.x, i0.y, i1.z), 0).rgb;
    let c011 = textureLoad(table, vec3<i32>(i0.x, i1.y, i1.z), 0).rgb;
    let c111 = textureLoad(table, vec3<i32>(i1.x, i1.y, i1.z), 0).rgb;
    let c00 = mix(c000, c100, f.x);
    let c10 = mix(c010, c110, f.x);
    let c01 = mix(c001, c101, f.x);
    let c11 = mix(c011, c111, f.x);
    return mix(mix(c00, c10, f.y), mix(c01, c11, f.y), f.z);
}

// Blend formulas, per channel (b: backdrop, s: source), from blend.rs.

fn screen(b: f32, s: f32) -> f32 {
    return b + s - b * s;
}

fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        return b * 2.0 * s;
    }
    return screen(b, 2.0 * s - 1.0);
}

fn color_dodge(b: f32, s: f32) -> f32 {
    if b <= 0.0 {
        return 0.0;
    }
    if s >= 1.0 {
        return 1.0;
    }
    return min(b / (1.0 - s), 1.0);
}

fn color_burn(b: f32, s: f32) -> f32 {
    if b >= 1.0 {
        return 1.0;
    }
    if s <= 0.0 {
        return 0.0;
    }
    return 1.0 - min((1.0 - b) / s, 1.0);
}

fn soft_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        return 2.0 * b * s + b * b * (1.0 - 2.0 * s);
    }
    return 2.0 * b * (1.0 - s) + sqrt(b) * (2.0 * s - 1.0);
}

fn separable(mode: u32, b: f32, s: f32) -> f32 {
    switch mode {
        case 1u: { return min(b, s); }
        case 2u: { return b * s; }
        case 3u: { return color_burn(b, s); }
        case 4u: { return max(b + s - 1.0, 0.0); }
        case 5u: { return max(b, s); }
        case 6u: { return screen(b, s); }
        case 7u: { return color_dodge(b, s); }
        case 8u: { return min(b + s, 1.0); }
        case 9u: { return hard_light(s, b); }
        case 10u: { return soft_light(b, s); }
        case 11u: { return hard_light(b, s); }
        case 12u: {
            if s <= 0.5 {
                return color_burn(b, 2.0 * s);
            }
            return color_dodge(b, 2.0 * s - 1.0);
        }
        case 13u: { return clamp(b + 2.0 * s - 1.0, 0.0, 1.0); }
        case 14u: {
            if s <= 0.5 {
                return min(b, 2.0 * s);
            }
            return max(b, 2.0 * s - 1.0);
        }
        case 15u: { return abs(b - s); }
        case 16u: { return b + s - 2.0 * b * s; }
        case 17u: { return max(b - s, 0.0); }
        case 18u: {
            if s <= 0.0 {
                return select(1.0, 0.0, b <= 0.0);
            }
            return min(b / s, 1.0);
        }
        case 19u: { return clamp(b - s + 0.5, 0.0, 1.0); }
        case 20u: { return clamp(b + s - 0.5, 0.0, 1.0); }
        default: { return s; }
    }
}

// Non-separable modes, from the W3C compositing spec.

fn lum(c: vec3<f32>) -> f32 {
    return 0.3 * c.r + 0.59 * c.g + 0.11 * c.b;
}

fn clip_color(c: vec3<f32>) -> vec3<f32> {
    let l = lum(c);
    let n = min(c.r, min(c.g, c.b));
    let x = max(c.r, max(c.g, c.b));
    var out = c;
    if n < 0.0 {
        out = l + (out - l) * l / (l - n);
    }
    if x > 1.0 {
        out = l + (out - l) * (1.0 - l) / (x - l);
    }
    return out;
}

fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> {
    return clip_color(c + (l - lum(c)));
}

fn sat(c: vec3<f32>) -> f32 {
    return max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b));
}

fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let hi = max(c.r, max(c.g, c.b));
    let lo = min(c.r, min(c.g, c.b));
    if hi <= lo {
        return vec3<f32>(0.0);
    }
    return (c - lo) * s / (hi - lo);
}

fn blend(mode: u32, cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 21u: { return set_lum(set_sat(cs, sat(cb)), lum(cb)); }
        case 22u: { return set_lum(set_sat(cb, sat(cs)), lum(cb)); }
        case 23u: { return set_lum(cs, lum(cb)); }
        case 24u: { return set_lum(cb, lum(cs)); }
        default: {
            return vec3<f32>(
                separable(mode, cb.r, cs.r),
                separable(mode, cb.g, cs.g),
                separable(mode, cb.b, cs.b),
            );
        }
    }
}

@fragment
fn layer(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let r = vec2<i32>(position.xy);
    var dst = vec4<f32>(0.0);
    if p.kind != 2u {
        dst = textureLoad(below, r, 0);
    }
    let q = r + p.region_origin - p.offset;
    var src = vec4<f32>(0.0);
    var coverage = 0.0;
    if p.transformed == 1u {
        let sampled = transformed_at(r + p.region_origin);
        src = sampled.colour;
        coverage = p.opacity * sampled.mask;
    } else {
        coverage = p.opacity * mask_at(q);
    }
    let cb = dst.rgb;
    let a_b = dst.a;

    // An adjustment blends the adjusted colour onto the original, leaving
    // transparency as it is (composite.rs `blend_tile`).
    if p.kind == 1u {
        if a_b <= 0.0 || coverage <= 0.0 {
            return dst;
        }
        let blended = blend(p.mode, cb, lookup(lut, p.lut_size, cb));
        return vec4<f32>(cb + (blended - cb) * coverage, a_b);
    }

    // Pixels: source-over with the blend mode (composite.rs `blend_source`).
    if p.transformed == 0u {
        src = pixel_at(q);
    }
    let a_s = src.a * coverage;
    if a_s <= 0.0 {
        return dst;
    }
    let cs = src.rgb;
    var blended = cs;
    if a_b > 0.0 {
        blended = blend(p.mode, cb, cs);
    }
    let a_o = a_s + a_b * (1.0 - a_s);
    let co = a_s * (1.0 - a_b) * cs + a_s * a_b * blended + (1.0 - a_s) * a_b * cb;
    return vec4<f32>(co / a_o, a_o);
}

struct Display {
    // Physical pixel where level pixel (0, 0) is drawn, and physical
    // pixels per level pixel.
    origin: vec2<f32>,
    scale: f32,
    // Interpolate between texels (zoomed out) rather than nearest.
    interpolate: u32,
    region: vec4<i32>,
    lut_size: u32,
    // The target stores linear values (an sRGB format), so encode less.
    linear_target: u32,
    pad: vec2<u32>,
};

// Group 1, so as not to share bindings with the layer pass's group 0.
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var display_lut: texture_3d<f32>;
@group(1) @binding(2) var<uniform> d: Display;

fn texel(t: vec2<i32>) -> vec4<f32> {
    let c = clamp(t, vec2<i32>(0), d.region.zw - vec2<i32>(1));
    return textureLoad(image, c, 0);
}

// Premultiplied, for interpolating colours with different alphas.
fn premultiplied(t: vec2<i32>) -> vec4<f32> {
    let c = texel(t);
    return vec4<f32>(c.rgb * c.a, c.a);
}

@fragment
fn display(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let at = (position.xy - d.origin) / d.scale - vec2<f32>(d.region.xy);
    if any(at < vec2<f32>(0.0)) || any(at >= vec2<f32>(d.region.zw)) {
        discard;
    }
    var c: vec4<f32>;
    if d.interpolate == 1u {
        let x = at - 0.5;
        let i = vec2<i32>(floor(x));
        let f = x - floor(x);
        let top = mix(premultiplied(i), premultiplied(i + vec2<i32>(1, 0)), f.x);
        let bottom = mix(premultiplied(i + vec2<i32>(0, 1)), premultiplied(i + vec2<i32>(1, 1)), f.x);
        c = mix(top, bottom, f.y);
    } else {
        c = premultiplied(vec2<i32>(floor(at)));
    }
    if c.a <= 0.0 {
        discard;
    }
    var rgb = lookup(display_lut, d.lut_size, c.rgb / c.a);
    if d.linear_target == 1u {
        rgb = pow(rgb, vec3<f32>(2.2));
    }
    // egui blends premultiplied colours.
    return vec4<f32>(rgb * c.a, c.a);
}
