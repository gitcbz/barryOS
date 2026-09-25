// Host-side driver for the image decoders.
//
// A decoder is the kind of code where being wrong and being right look the
// same from the outside: a picture comes out, it has the right dimensions, and
// the colours are nearly the colours.  Nothing about that says whether the
// coefficients were read in the right order or whether the last row was
// written from the wrong buffer.  So this decodes a file with the kernel's own
// source and writes the pixels somewhere else, where they can be compared
// against a decoder that was written by somebody else entirely.
//
//     tests/run-img-host.sh a.png b.jpg c.gif
//
// Each file is decoded and written beside itself as a PPM, with one line of
// text saying what came out.
//
// Everything under test is the kernel's own file, reached by `#[path]`.

extern crate alloc;

mod serial {
    pub fn print_str(s: &str) { print!("{}", s); }
    pub fn print_dec(v: u64) { print!("{}", v); }
}

#[path = "../kernel/src/apps/web/img/mod.rs"]
mod img;

/// Colour the decoder composites transparency onto.  Matches what the checker
/// uses as its reference background, because a half-transparent pixel is only
/// meaningful next to the thing it was blended with.
const BG: (u8, u8, u8) = (255, 255, 255);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let budget: usize = std::env::var("BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(img::IMAGE_BUDGET);

    let mut failed = 0usize;
    for path in &args {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                println!("{}\tREAD-ERROR\t{}", path, e);
                failed += 1;
                continue;
            }
        };
        match img::decode(&bytes, budget, BG) {
            Ok(im) => {
                // The mean is here because a decoder can be wrong in a way
                // that still produces a plausible picture: an image that came
                // out inverted, or all one colour, has the right shape and
                // nothing else.
                let (r, g, b) = im.mean();
                println!("{}\t{}\t{}\t{}\t{}\t{}\t{}", path, im.w, im.h, r, g, b, im.bytes());
                if let Err(e) = write_ppm(&format!("{}.ppm", path), &im) {
                    eprintln!("could not write the ppm: {}", e);
                }
            }
            Err(e) => {
                println!("{}\tERROR\t{}", path, e);
                failed += 1;
            }
        }
    }
    if failed != 0 {
        println!("{} file(s) failed", failed);
    }
}

fn write_ppm(path: &str, im: &img::Image) -> std::io::Result<()> {
    let mut out = Vec::with_capacity(im.px.len() + 32);
    out.extend_from_slice(format!("P6\n{} {}\n255\n", im.w, im.h).as_bytes());
    out.extend_from_slice(&im.px);
    std::fs::write(path, out)
}
