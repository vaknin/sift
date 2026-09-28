use std::time::Instant;
use sift_engine::{face::Models, preview};
fn main() -> anyhow::Result<()> {
    let dir = std::env::args().nth(1).unwrap();
    let t = Instant::now();
    let m = Models::new()?;
    println!("models {:?}", t.elapsed());
    let shots = preview::scan(dir.as_ref())?;
    println!("scan {:?}", t.elapsed());
    for s in shots.iter().skip(80).take(4) {
        let t = Instant::now();
        let img = preview::load(s)?;
        let a = t.elapsed();
        let d = m.detect(&img, 0.6)?;
        let b = t.elapsed();
        if let Some(f) = d.first() {
            let mesh = m.mesh(&img, f)?;
            let c = t.elapsed();
            sift_engine::measure::measure(&img, f, &mesh);
            println!("load {a:?} detect {:?} mesh {:?} measure {:?}", b - a, c - b, t.elapsed() - c);
        }
    }
    Ok(())
}
