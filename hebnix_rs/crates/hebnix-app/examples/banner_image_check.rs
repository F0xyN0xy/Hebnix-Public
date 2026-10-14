//! Run with an input banner and output PNG, or `cargo test --example banner_image_check`.
#[path = "../src/ui/workshop/banner_image.rs"]
mod banner_image;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err("usage: banner_image_check <input image> <output image>".into());
    }
    let bytes = banner_image::prepare(std::fs::read(&args[1])?)?;
    std::fs::write(&args[2], bytes)?;
    Ok(())
}

#[cfg(test)]
#[path = "support/banner_image_tests.rs"]
mod tests;
