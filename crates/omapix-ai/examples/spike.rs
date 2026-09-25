//! Milestone 0 spike: run darktable's SAM 2.1 on a photo, on the CPU and
//! with CUDA, and time it. `cargo run --release -p omapix-ai --example
//! spike -- photo.jpg x y out.png`
use std::time::Instant;

use omapix_engine::DisplayTransform;
use ort::ep;
use ort::session::Session;
use ort::value::Tensor;

const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (path, px, py, out) = (&args[1], args[2].parse::<f32>()?, args[3].parse::<f32>()?, &args[4]);
    ort::init_from("/usr/lib/libonnxruntime.so")?.commit();
    let dir = std::path::Path::new(&std::env::var("HOME")?).join(".local/share/darktable/models/mask-object-sam21-small");

    let t = Instant::now();
    let doc = omapix_engine::io::load(std::path::Path::new(path))?;
    let image = doc.composite();
    let (w, h) = (image.width(), image.height());
    let mut srgb = vec![[0u8; 4]; image.pixels().len()];
    DisplayTransform::to_srgb(&doc.profile)?.convert(image.pixels(), &mut srgb);
    // Stretched to 1024 × 1024, normalised, channels first.
    let n = 1024usize;
    let mut input = vec![0f32; 3 * n * n];
    for y in 0..n {
        for x in 0..n {
            let sx = ((x as f32 + 0.5) * w as f32 / n as f32) as usize;
            let sy = ((y as f32 + 0.5) * h as f32 / n as f32) as usize;
            let p = srgb[sy.min(h as usize - 1) * w as usize + sx.min(w as usize - 1)];
            for c in 0..3 {
                input[c * n * n + y * n + x] = (p[c] as f32 / 255.0 - MEAN[c]) / STD[c];
            }
        }
    }
    println!("{w}×{h}: loaded and prepared in {:?}", t.elapsed());

    for cuda in [false, true] {
        let builder = || -> ort::Result<_> {
            let b = Session::builder()?;
            Ok(if cuda { b.with_execution_providers([ep::CUDA::default().build().error_on_failure()])? } else { b })
        };
        let label = if cuda { "CUDA" } else { "CPU" };
        let t = Instant::now();
        let mut encoder = builder()?.commit_from_file(dir.join("encoder.onnx"))?;
        let mut decoder = builder()?.commit_from_file(dir.join("decoder.onnx"))?;
        println!("{label}: sessions in {:?}", t.elapsed());
        for run in 0..3 {
            let t = Instant::now();
            let image = Tensor::from_array((vec![1i64, 3, n as i64, n as i64], input.clone()))?;
            let encoded = encoder.run(ort::inputs!["image" => image])?;
            let encode = t.elapsed();
            let feats: Vec<(Vec<i64>, Vec<f32>)> = ["image_embed", "high_res_feats_0", "high_res_feats_1"]
                .iter()
                .map(|k| {
                    let (shape, data) = encoded[*k].try_extract_tensor::<f32>().unwrap();
                    (shape.to_vec(), data.to_vec())
                })
                .collect();
            drop(encoded);
            let t = Instant::now();
            let coords = Tensor::from_array((vec![1i64, 1, 2], vec![px * n as f32 / w as f32, py * n as f32 / h as f32]))?;
            let labels = Tensor::from_array((vec![1i64, 1], vec![1f32]))?;
            let mask_input = Tensor::from_array((vec![1i64, 1, 256, 256], vec![0f32; 256 * 256]))?;
            let has_mask = Tensor::from_array((vec![1i64], vec![0f32]))?;
            let [embed, f0, f1] = [0, 1, 2].map(|i| Tensor::from_array(feats[i].clone()).unwrap());
            let decoded = decoder.run(ort::inputs![
                "image_embed" => embed,
                "high_res_feats_0" => f0,
                "high_res_feats_1" => f1,
                "point_coords" => coords,
                "point_labels" => labels,
                "mask_input" => mask_input,
                "has_mask_input" => has_mask,
            ])?;
            let decode = t.elapsed();
            let (_, iou) = decoded["iou_predictions"].try_extract_tensor::<f32>()?;
            let (shape, masks) = decoded["masks"].try_extract_tensor::<f32>()?;
            let best = (0..iou.len()).max_by(|&a, &b| iou[a].total_cmp(&iou[b])).unwrap();
            println!("{label} run {run}: encode {encode:?}, decode {decode:?}, masks {shape:?}, iou {iou:?}");
            if cuda && run == 2 {
                // The best mask over a small copy of the photo, for looking at.
                let (ow, oh) = (600u32, 600 * h / w);
                let plane = &masks[best * 256 * 256..(best + 1) * 256 * 256];
                let mut img = image::RgbImage::new(ow, oh);
                for (x, y, pixel) in img.enumerate_pixels_mut() {
                    let s = srgb[(y * h / oh * w + x * w / ow) as usize];
                    let m = plane[(y * 256 / oh * 256 + x * 256 / ow) as usize] > 0.0;
                    let k = |v: u8, r: u8| if m { v / 2 + r / 2 } else { v };
                    *pixel = image::Rgb([k(s[0], 255), k(s[1], 0), k(s[2], 0)]);
                }
                img.save(out)?;
            }
        }
    }
    Ok(())
}
