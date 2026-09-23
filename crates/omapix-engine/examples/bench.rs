//! Times core operations on a real image:
//! `cargo run --release -p omapix-engine --example bench -- photo.tif`

use std::time::Instant;

use omapix_engine::{filters, io, ops, tiled::Tiled};

fn main() {
    let path = std::env::args().nth(1).expect("usage: bench <image>");
    let t = Instant::now();
    let mut doc = io::load(path.as_ref()).expect("load");
    println!(
        "load            {:>8.0?}  {}x{}",
        t.elapsed(),
        doc.width,
        doc.height
    );

    let t = Instant::now();
    let _ = doc.composite();
    println!("composite x1    {:>8.0?}", t.elapsed());

    let t = Instant::now();
    let _ = filters::gaussian_blur(&doc.layers[0].pixels, 9.0);
    println!("gaussian r9     {:>8.0?}", t.elapsed());

    let t = Instant::now();
    ops::frequency_separation(&mut doc, 0, 9.0);
    println!("freq sep        {:>8.0?}", t.elapsed());

    let t = Instant::now();
    ops::dodge_and_burn_layer(&mut doc, 2);
    println!("dodge&burn      {:>8.0?}", t.elapsed());

    let t = Instant::now();
    let _ = doc.composite();
    println!("composite x4    {:>8.0?}", t.elapsed());

    let t = Instant::now();
    let snapshot = doc.clone();
    println!("undo snapshot   {:>8.0?}", t.elapsed());
    assert!(snapshot.layers[0].pixels.same_tiles(&doc.layers[0].pixels));

    let t = Instant::now();
    let _ = Tiled::from_raster(&doc.composite());
    println!("stamp visible   {:>8.0?}", t.elapsed());
}
