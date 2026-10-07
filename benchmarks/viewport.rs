use std::time::Instant;

fn main() {
    let cells: usize = 160 * 50;
    let frames: u32 = std::env::args().nth(1).unwrap_or("30000".into()).parse().unwrap();
    let base: Vec<u32> = (0..cells).map(|i| (32 + (i * 13 + i / 160 * 7) % 95) as u32 | ((i / 160 % 8) << 8) as u32).collect();
    let mut previous = vec![0_u32; cells];
    let mut next = vec![0_u32; cells];
    let mut seed = 123456789_u32;
    let mut checksum = 2166136261_u32;
    let mut changed: u64 = 0;
    let start = Instant::now();
    for _ in 0..frames {
        next.copy_from_slice(&base);
        for _ in 0..8 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let pos = cells - 320 + (seed % 320) as usize;
            next[pos] = (32 + ((seed >> 16) % 95)) | (7 << 8);
        }
        for (i, (&a, &b)) in next.iter().zip(previous.iter()).enumerate() {
            if a != b {
                changed += 1;
                checksum = (checksum ^ a ^ i as u32).wrapping_mul(16777619);
            }
        }
        std::mem::swap(&mut previous, &mut next);
    }
    println!("{{\"frames\":{frames},\"kernel_ms\":{},\"checksum\":{checksum},\"changed\":{changed}}}",start.elapsed().as_secs_f64()*1000.0);
}
