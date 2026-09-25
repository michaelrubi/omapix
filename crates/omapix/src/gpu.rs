//! The GPU side of live compositing (see `live.rs`): a stack's textures
//! are uploaded once, then each frame one render pass per layer blends it
//! onto what's below (`live.wgsl`), and an egui paint callback draws the
//! result on the canvas through the display colour transform.

use std::sync::{Arc, OnceLock};

use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, ScreenDescriptor};
use eframe::wgpu;
use omapix_engine::{DisplayTransform, Pixel};
use wgpu::util::DeviceExt;

use crate::live::{LUT_SIZE, LiveFrame, Plane, Settings, Source, Table};

/// The largest texture side the GPU takes, once live compositing is set up
/// at startup.
static MAX_SIDE: OnceLock<u32> = OnceLock::new();

/// Set up live compositing on eframe's GPU.
pub fn install(render_state: &egui_wgpu::RenderState) {
    let device = &render_state.device;
    let gpu = LiveGpu::new(device, render_state.target_format);
    render_state.renderer.write().callback_resources.insert(gpu);
    let _ = MAX_SIDE.set(device.limits().max_texture_dimension_2d);
}

/// The largest texture side for live compositing, or `None` without it
/// (headless, as in tests).
pub fn max_side() -> Option<u32> {
    MAX_SIDE.get().copied()
}

/// Points per side of the display transform's lookup table.
pub const DISPLAY_LUT_SIZE: usize = 65;

/// The display transform sampled on a grid, for [`LiveDraw`].
pub fn display_lut(transform: &DisplayTransform) -> Vec<[f32; 4]> {
    let n = DISPLAY_LUT_SIZE;
    let step = 65535.0 / (n - 1) as f32;
    let at = |k: usize| (k as f32 * step).round() as u16;
    let src: Vec<Pixel> = (0..n * n * n)
        .map(|i| [at(i % n), at(i / n % n), at(i / (n * n)), u16::MAX])
        .collect();
    let mut out = vec![[0u8; 4]; src.len()];
    transform.convert(&src, &mut out);
    out.iter()
        .map(|c| [c[0], c[1], c[2], 255].map(|v| f32::from(v) / 255.0))
        .collect()
}

/// What to draw this frame: a stack, where its subject is (in level
/// pixels) and its settings, and where on screen (in physical pixels)
/// level pixel (0, 0) goes and how big level pixels are.
pub struct LiveDraw {
    pub frame: LiveFrame,
    pub display_lut: Arc<Vec<[f32; 4]>>,
    pub origin: [f32; 2],
    pub scale: f32,
}

/// Pipelines, made once at startup, and the stack last uploaded.
pub struct LiveGpu {
    layer_pipeline: wgpu::RenderPipeline,
    display_pipeline: wgpu::RenderPipeline,
    layer_layout: wgpu::BindGroupLayout,
    display_layout: wgpu::BindGroupLayout,
    /// Stand-ins for bindings a pass doesn't use.
    empty_pixels: wgpu::TextureView,
    empty_mask: wgpu::TextureView,
    empty_lut: wgpu::TextureView,
    linear_target: bool,
    uploaded: Option<Uploaded>,
}

struct Uploaded {
    id: u64,
    passes: Vec<Pass>,
    /// Ping-pong targets: pass `i` writes `targets[i % 2]`.
    targets: [(wgpu::Texture, wgpu::TextureView); 2],
    display_uniform: wgpu::Buffer,
    display_group: wgpu::BindGroup,
}

struct Pass {
    uniform: wgpu::Buffer,
    group: wgpu::BindGroup,
    params: Params,
    /// The subject's pass, and its adjustment's table as last uploaded.
    subject: Option<(wgpu::Texture, Option<Table>)>,
}

/// `Layer` in live.wgsl.
#[derive(Clone, Copy)]
struct Params {
    region_origin: [i32; 2],
    plane: ([i32; 2], [i32; 2]),
    mask: ([i32; 2], [i32; 2]),
    pixel_fill: [f32; 4],
    mode: u32,
    kind: u32,
    has_mask: bool,
    opacity: f32,
    mask_fill: f32,
}

impl Params {
    fn bytes(&self, offset: [i32; 2]) -> Vec<u8> {
        let mut words: Vec<u32> = Vec::with_capacity(24);
        for v in [
            self.region_origin,
            offset,
            self.plane.0,
            self.plane.1,
            self.mask.0,
            self.mask.1,
        ] {
            words.extend(v.map(|x| x as u32));
        }
        words.extend(self.pixel_fill.map(f32::to_bits));
        words.extend([self.mode, self.kind, self.has_mask.into(), LUT_SIZE as u32]);
        words.extend([self.opacity, self.mask_fill, 0.0, 0.0].map(f32::to_bits));
        bytemuck::cast_slice(&words).to_vec()
    }
}

const TARGET: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

impl LiveGpu {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("live"),
            source: wgpu::ShaderSource::Wgsl(include_str!("live.wgsl").into()),
        });
        let texture = |binding, sample_type, dim| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: dim,
                multisampled: false,
            },
            count: None,
        };
        let uniform = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let float = wgpu::TextureSampleType::Float { filterable: false };
        let uint = wgpu::TextureSampleType::Uint;
        let (d2, d3) = (wgpu::TextureViewDimension::D2, wgpu::TextureViewDimension::D3);
        let layer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("live layer"),
            entries: &[
                texture(0, float, d2),
                texture(1, uint, d2),
                texture(2, uint, d2),
                texture(3, float, d3),
                uniform(4),
            ],
        });
        let display_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("live display"),
            entries: &[texture(0, float, d2), texture(1, float, d3), uniform(2)],
        });
        let pipeline = |layouts: &[Option<&wgpu::BindGroupLayout>],
                        entry: &str,
                        format: wgpu::TextureFormat,
                        blend: Option<wgpu::BlendState>| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(entry),
                bind_group_layouts: layouts,
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let layer_pipeline = pipeline(&[Some(&layer_layout)], "layer", TARGET, None);
        let display_pipeline = pipeline(
            &[None, Some(&display_layout)],
            "display",
            target_format,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );
        let empty = |format, dimension| {
            texture_2d_or_3d(device, format, dimension, (1, 1, 1))
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        Self {
            layer_pipeline,
            display_pipeline,
            layer_layout,
            display_layout,
            empty_pixels: empty(wgpu::TextureFormat::Rgba16Uint, wgpu::TextureDimension::D2),
            empty_mask: empty(wgpu::TextureFormat::R16Uint, wgpu::TextureDimension::D2),
            empty_lut: empty(wgpu::TextureFormat::Rgba32Float, wgpu::TextureDimension::D3),
            linear_target: target_format.is_srgb(),
            uploaded: None,
        }
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, draw: &LiveDraw) {
        let stack = &draw.frame.stack;
        let (x0, y0, w, h) = stack.region;
        let target = || {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("live target"),
                size: extent((w, h, 1)),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TARGET,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            (texture, view)
        };
        let targets = [target(), target()];
        let region_origin = [x0 as i32, y0 as i32];
        let rect = |x0: u32, y0: u32, w: u32, h: u32| ([x0 as i32, y0 as i32], [w as i32, h as i32]);

        // What's below comes first, onto nothing, then each layer.
        let below = Params {
            region_origin,
            plane: rect(stack.below.x0, stack.below.y0, stack.below.w, stack.below.h),
            mask: ([0; 2], [0; 2]),
            pixel_fill: unit(stack.below.fill),
            mode: 0,
            kind: 2,
            has_mask: false,
            opacity: 1.0,
            mask_fill: 1.0,
        };
        let mut sources = vec![(below, Some(upload_pixels(device, queue, &stack.below)), None, None, false)];
        let table_texture = |data: &[[f32; 3]]| {
            let texture = lut_texture(device, LUT_SIZE);
            write_lut(queue, &texture, data, LUT_SIZE);
            texture
        };
        for layer in &stack.layers {
            let (pixels, lut, plane, fill, kind) = match &layer.source {
                Source::Pixels(plane) => (
                    Some(upload_pixels(device, queue, plane)),
                    None,
                    rect(plane.x0, plane.y0, plane.w, plane.h),
                    unit(plane.fill),
                    0,
                ),
                Source::Adjustment(table) => {
                    (None, Some(table_texture(table)), ([0; 2], [0; 2]), [0.0; 4], 1)
                }
            };
            let mask = layer.mask.as_ref().map(|m| {
                let texture = upload(device, queue, wgpu::TextureFormat::R16Uint, (m.w, m.h), &m.data);
                (texture, rect(m.x0, m.y0, m.w, m.h), f32::from(m.fill) / 65535.0)
            });
            let params = Params {
                region_origin,
                plane,
                mask: mask.as_ref().map_or(([0; 2], [0; 2]), |m| m.1),
                pixel_fill: fill,
                mode: layer.mode as u32,
                kind,
                has_mask: mask.is_some(),
                opacity: layer.opacity,
                mask_fill: mask.as_ref().map_or(1.0, |m| m.2),
            };
            sources.push((params, pixels, mask.map(|m| m.0), lut, layer.subject));
        }

        let passes = sources
            .into_iter()
            .enumerate()
            .map(|(i, (params, pixels, mask, lut, subject))| {
                let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("live layer"),
                    contents: &params.bytes([0, 0]),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                });
                let view = |v: Option<wgpu::TextureView>, empty: &wgpu::TextureView| v.unwrap_or_else(|| empty.clone());
                let lut_view = lut.as_ref().map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()));
                let (pixels, mask, lut_view) = (
                    view(pixels, &self.empty_pixels),
                    view(mask, &self.empty_mask),
                    view(lut_view, &self.empty_lut),
                );
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("live layer"),
                    layout: &self.layer_layout,
                    entries: &[
                        entry(0, &targets[(i + 1) % 2].1),
                        entry(1, &pixels),
                        entry(2, &mask),
                        entry(3, &lut_view),
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: uniform.as_entire_binding(),
                        },
                    ],
                });
                Pass {
                    uniform,
                    group,
                    params,
                    subject: subject.then(|| (lut.unwrap_or_else(|| table_texture(&[])), None)),
                }
            })
            .collect::<Vec<_>>();

        let display_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("live display"),
            size: 48,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let display_lut = lut_texture(device, DISPLAY_LUT_SIZE);
        write(queue, &display_lut, bytemuck::cast_slice(draw.display_lut.as_slice()), DISPLAY_LUT_SIZE as u32 * 16, (DISPLAY_LUT_SIZE as u32, DISPLAY_LUT_SIZE as u32, DISPLAY_LUT_SIZE as u32));
        let display_lut = display_lut.create_view(&wgpu::TextureViewDescriptor::default());
        let display_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("live display"),
            layout: &self.display_layout,
            entries: &[
                entry(0, &targets[(passes.len() - 1) % 2].1),
                entry(1, &display_lut),
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: display_uniform.as_entire_binding(),
                },
            ],
        });
        self.uploaded = Some(Uploaded {
            id: stack.id,
            passes,
            targets,
            display_uniform,
            display_group,
        });
    }
}

impl CallbackTrait for LiveDraw {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(gpu) = resources.get_mut::<LiveGpu>() else {
            return Vec::new();
        };
        let stack = &self.frame.stack;
        if gpu.uploaded.as_ref().is_none_or(|u| u.id != stack.id) {
            gpu.upload(device, queue, self);
        }
        let uploaded = gpu.uploaded.as_mut().expect("just uploaded");
        let offset = [self.frame.offset.0, self.frame.offset.1];
        for (i, pass) in uploaded.passes.iter_mut().enumerate() {
            let mut params = pass.params;
            let mut moved = [0, 0];
            if let Some((lut, uploaded_lut)) = &mut pass.subject {
                moved = offset;
                if let Some(Settings { opacity, mode, lut: table }) = &self.frame.settings {
                    (params.opacity, params.mode) = (*opacity, *mode as u32);
                    // An adjustment's new table, when it's changed.
                    if let Some(table) = table
                        && !uploaded_lut.as_ref().is_some_and(|u| Arc::ptr_eq(u, table))
                    {
                        write_lut(queue, lut, table, LUT_SIZE);
                        *uploaded_lut = Some(Arc::clone(table));
                    }
                }
            }
            queue.write_buffer(&pass.uniform, 0, &params.bytes(moved));
            let mut render = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("live layer"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &uploaded.targets[i % 2].1,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            render.set_pipeline(&gpu.layer_pipeline);
            render.set_bind_group(0, &pass.group, &[]);
            render.draw(0..3, 0..1);
        }
        let (x0, y0, w, h) = stack.region;
        let mut words: Vec<u32> = Vec::with_capacity(12);
        words.extend([self.origin[0], self.origin[1], self.scale].map(f32::to_bits));
        words.push((self.scale < 1.0).into());
        words.extend([x0, y0, w, h]);
        words.extend([DISPLAY_LUT_SIZE as u32, gpu.linear_target.into(), 0, 0]);
        queue.write_buffer(&uploaded.display_uniform, 0, bytemuck::cast_slice(&words));
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(gpu) = resources.get::<LiveGpu>() else {
            return;
        };
        let Some(uploaded) = gpu.uploaded.as_ref().filter(|u| u.id == self.frame.stack.id) else {
            return;
        };
        render_pass.set_pipeline(&gpu.display_pipeline);
        render_pass.set_bind_group(1, &uploaded.display_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

impl LiveDraw {
    /// A paint callback drawing this over `rect` (the canvas).
    pub fn callback(self, rect: egui::Rect) -> egui::PaintCallback {
        egui_wgpu::Callback::new_paint_callback(rect, self)
    }
}

fn unit(p: Pixel) -> [f32; 4] {
    p.map(|v| f32::from(v) / 65535.0)
}

fn entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

fn extent((w, h, d): (u32, u32, u32)) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: w.max(1),
        height: h.max(1),
        depth_or_array_layers: d.max(1),
    }
}

fn texture_2d_or_3d(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    dimension: wgpu::TextureDimension,
    size: (u32, u32, u32),
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("live source"),
        size: extent(size),
        mip_level_count: 1,
        sample_count: 1,
        dimension,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// A 2D texture of `size` holding `data`, row-major.
fn upload<T: bytemuck::Pod>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    (w, h): (u32, u32),
    data: &[T],
) -> wgpu::TextureView {
    let texture = texture_2d_or_3d(device, format, wgpu::TextureDimension::D2, (w, h, 1));
    if w > 0 && h > 0 {
        write(queue, &texture, bytemuck::cast_slice(data), w * size_of::<T>() as u32, (w, h, 1));
    }
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

fn upload_pixels(device: &wgpu::Device, queue: &wgpu::Queue, plane: &Plane<Pixel>) -> wgpu::TextureView {
    upload(device, queue, wgpu::TextureFormat::Rgba16Uint, (plane.w, plane.h), &plane.data)
}

fn lut_texture(device: &wgpu::Device, n: usize) -> wgpu::Texture {
    let n = n as u32;
    texture_2d_or_3d(device, wgpu::TextureFormat::Rgba32Float, wgpu::TextureDimension::D3, (n, n, n))
}

/// Write an adjustment's lookup table into `texture`.
fn write_lut(queue: &wgpu::Queue, texture: &wgpu::Texture, table: &[[f32; 3]], n: usize) {
    if table.is_empty() {
        return;
    }
    let data: Vec<[f32; 4]> = table.iter().map(|c| [c[0], c[1], c[2], 1.0]).collect();
    let n = n as u32;
    write(queue, texture, bytemuck::cast_slice(&data), n * 16, (n, n, n));
}

fn write(queue: &wgpu::Queue, texture: &wgpu::Texture, bytes: &[u8], row: u32, size: (u32, u32, u32)) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row),
            rows_per_image: Some(size.1),
        },
        extent(size),
    );
}

#[cfg(test)]
mod tests {
    //! Run with `cargo test -p omapix gpu -- --ignored`: needs a GPU.
    use super::*;
    use crate::live;
    use omapix_engine::adjust::{Adjustment, Curves};
    use omapix_engine::layer::{Layer, Mask};
    use omapix_engine::tiled::Tiled;
    use omapix_engine::{BlendMode, ColorProfile, Document, Raster, composite};

    fn device() -> (wgpu::Device, wgpu::Queue) {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).expect("a GPU");
        pollster::block_on(adapter.request_device(&Default::default())).expect("a device")
    }

    /// The last pass's target, read back.
    fn composite_on_gpu(device: &wgpu::Device, queue: &wgpu::Queue, draw: LiveDraw) -> Vec<[f32; 4]> {
        let mut resources = CallbackResources::default();
        resources.insert(LiveGpu::new(device, wgpu::TextureFormat::Rgba8Unorm));
        let mut encoder = device.create_command_encoder(&Default::default());
        let screen = ScreenDescriptor {
            size_in_pixels: [1, 1],
            pixels_per_point: 1.0,
        };
        draw.prepare(device, queue, &screen, &mut encoder, &mut resources);
        let (_, _, w, h) = draw.frame.stack.region;
        let gpu = resources.get::<LiveGpu>().unwrap();
        let uploaded = gpu.uploaded.as_ref().unwrap();
        let last = &uploaded.targets[(uploaded.passes.len() - 1) % 2].0;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(w * h * 16),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            last.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 16),
                    rows_per_image: Some(h),
                },
            },
            extent((w, h, 1)),
        );
        queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let bytes = buffer.slice(..).get_mapped_range().expect("mapped");
        bytemuck::cast_slice(&bytes).to_vec()
    }

    /// A 64 × 40 document: a gradient, a half-transparent patch with a
    /// soft mask to move, a layer above in `mode`, and optionally a masked
    /// Curves on top.
    fn document(mode: BlendMode, curves: bool) -> Document {
        let (w, h) = (64u32, 40u32);
        let gradient: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                [(x * 1000) as u16, (y * 1600) as u16, 40000 - (x * 300) as u16, 65535]
            })
            .collect();
        let mut doc = Document::from_image("t.tif".into(), &Raster::new(w, h, gradient), ColorProfile::srgb(), 16);
        let patch: Vec<Pixel> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let inside = (10..40).contains(&x) && (8..30).contains(&y);
                if inside { [60000, (x * 900) as u16, 20000, 30000 + (y * 800) as u16] } else { [0; 4] }
            })
            .collect();
        let id = doc.next_layer_id();
        let mut layer = Layer::from_raster(id, "Patch", &Raster::new(w, h, patch));
        layer.blend = mode;
        let mut mask = Mask::white(w, h);
        mask.pixels = Tiled::from_slice(w, h, 65535, &(0..w * h).map(|i| (65535 - (i % w) * 600) as u16).collect::<Vec<_>>());
        layer.mask = Some(mask);
        doc.layers.push(layer);
        let id = doc.next_layer_id();
        let above: Vec<Pixel> = (0..w * h).map(|i| [20000, 50000, (i % 7 * 9000) as u16, 45000]).collect();
        let mut layer = Layer::from_raster(id, "Above", &Raster::new(w, h, above));
        layer.blend = mode;
        layer.opacity = 0.7;
        doc.layers.push(layer);
        if curves {
            let mut c = Curves::default();
            c.master.points.insert(1, (0.4, 0.55));
            let id = doc.next_layer_id();
            let mut layer = Layer::adjustment(id, Adjustment::Curves(c), w, h);
            layer.mask.as_mut().unwrap().pixels = Tiled::new(w, h, 40000);
            doc.layers.push(layer);
        }
        doc
    }

    /// The worst channel difference between the GPU's live composite of
    /// moving the patch by `offset` and the CPU's composite of it moved.
    fn worst_difference(device: &wgpu::Device, queue: &wgpu::Queue, doc: &Document, offset: (i32, i32)) -> f32 {
        let (w, h) = (doc.width, doc.height);
        let patch = doc.layers[1].id;
        let stack = live::build(doc, &doc.layers, patch, true, 0, (0, 0, w, h)).expect("shown live");
        let draw = LiveDraw {
            frame: LiveFrame {
                stack: Arc::new(stack),
                offset,
                settings: None,
            },
            display_lut: Arc::new(vec![[0.0; 4]; DISPLAY_LUT_SIZE.pow(3)]),
            origin: [0.0; 2],
            scale: 1.0,
        };
        let gpu = composite_on_gpu(device, queue, draw);
        let mut moved = doc.clone();
        let layer = &mut moved.layers[1];
        layer.pixels = layer.pixels.translated(offset.0, offset.1, [0; 4]);
        let mask = layer.mask.as_mut().unwrap();
        mask.pixels = mask.pixels.translated(offset.0, offset.1, 65535);
        let cpu = composite::composite(&moved.layers, w, h);
        cpu.pixels()
            .iter()
            .zip(&gpu)
            .flat_map(|(c, g)| (0..4).map(move |i| (f32::from(c[i]) / 65535.0 - g[i]).abs()))
            .fold(0.0, f32::max)
    }

    #[test]
    #[ignore]
    fn live_composite_matches_the_cpu_for_every_blend_mode() {
        let (device, queue) = device();
        for &mode in BlendMode::MENU.iter().flat_map(|g| g.iter()) {
            let worst = worst_difference(&device, &queue, &document(mode, false), (5, -3));
            assert!(worst < 2e-4, "{mode:?}: off by {worst}");
        }
        // Adjustments go through a lookup table, so are a little less exact.
        let worst = worst_difference(&device, &queue, &document(BlendMode::SoftLight, true), (-7, 4));
        assert!(worst < 4e-3, "Curves: off by {worst}");
    }

    #[test]
    #[ignore]
    fn live_display_matches_the_cpu_display() {
        let (device, queue) = device();
        let doc = document(BlendMode::Overlay, true);
        let (w, h) = (doc.width, doc.height);
        let transform = DisplayTransform::to_srgb(&doc.profile).unwrap();
        let stack = live::build(&doc, &doc.layers, doc.layers[1].id, true, 0, (0, 0, w, h)).unwrap();
        let draw = LiveDraw {
            frame: LiveFrame {
                stack: Arc::new(stack),
                offset: (0, 0),
                settings: None,
            },
            display_lut: Arc::new(display_lut(&transform)),
            origin: [0.0; 2],
            scale: 1.0,
        };
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut resources = CallbackResources::default();
        resources.insert(LiveGpu::new(&device, format));
        let mut encoder = device.create_command_encoder(&Default::default());
        let screen = ScreenDescriptor {
            size_in_pixels: [w, h],
            pixels_per_point: 1.0,
        };
        draw.prepare(&device, &queue, &screen, &mut encoder, &mut resources);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent((w, h, 1)),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let mut pass = pass.forget_lifetime();
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w as f32, h as f32));
            let info = egui::PaintCallbackInfo {
                viewport: rect,
                clip_rect: rect,
                pixels_per_point: 1.0,
                screen_size_px: [w, h],
            };
            draw.paint(info, &mut pass, &resources);
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(w * h * 4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
            },
            extent((w, h, 1)),
        );
        queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let gpu: Vec<[u8; 4]> = bytemuck::cast_slice(&buffer.slice(..).get_mapped_range().unwrap()).to_vec();
        let flat = composite::composite(&doc.layers, w, h);
        let mut cpu = vec![[0u8; 4]; (w * h) as usize];
        transform.convert(flat.pixels(), &mut cpu);
        let worst = cpu
            .iter()
            .zip(&gpu)
            .flat_map(|(c, g)| (0..4).map(move |i| c[i].abs_diff(g[i])))
            .max()
            .unwrap();
        // The lookup table is interpolated between 65 points a side.
        assert!(worst <= 2, "off by {worst} levels");
    }

    #[test]
    #[ignore]
    fn live_edits_match_the_cpu() {
        let (device, queue) = device();
        let doc = document(BlendMode::Normal, true);
        let (w, h) = (doc.width, doc.height);
        let curves = doc.layers[3].id;
        // Built before the edit, as a live edit's stack is.
        let stack = Arc::new(live::build(&doc, &doc.layers, curves, false, 0, (0, 0, w, h)).unwrap());
        let mut edited = doc.clone();
        let layer = &mut edited.layers[3];
        let mut c = Curves::default();
        c.master.points.insert(1, (0.5, 0.3));
        layer.adjustment = Some(Adjustment::Curves(c));
        layer.opacity = 0.6;
        layer.blend = BlendMode::Luminosity;
        let settings = Settings {
            opacity: 0.6,
            mode: BlendMode::Luminosity,
            lut: Some(Arc::new(layer.adjustment.as_ref().unwrap().prepare().lut(LUT_SIZE))),
        };
        let draw = LiveDraw {
            frame: LiveFrame {
                stack,
                offset: (0, 0),
                settings: Some(settings),
            },
            display_lut: Arc::new(vec![[0.0; 4]; DISPLAY_LUT_SIZE.pow(3)]),
            origin: [0.0; 2],
            scale: 1.0,
        };
        let gpu = composite_on_gpu(&device, &queue, draw);
        let cpu = composite::composite(&edited.layers, w, h);
        let worst = cpu
            .pixels()
            .iter()
            .zip(&gpu)
            .flat_map(|(c, g)| (0..4).map(move |i| (f32::from(c[i]) / 65535.0 - g[i]).abs()))
            .fold(0.0, f32::max);
        assert!(worst < 4e-3, "off by {worst}");
    }
}
