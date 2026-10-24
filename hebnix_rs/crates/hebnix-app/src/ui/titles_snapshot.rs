//! Small test-only rasterizer for inspecting the actual egui view without
//! starting Hebnix services, a window, or touching installed game files.
use eframe::egui;
use std::{collections::HashMap, path::Path};

pub fn save(ctx: &egui::Context, frames: &[egui::FullOutput], output: &Path) {
    let mut textures = HashMap::<egui::TextureId, egui::ColorImage>::new();
    for frame in frames {
        for (id, delta) in &frame.textures_delta.set {
            let egui::ImageData::Color(image) = &delta.image;
            if let Some([x, y]) = delta.pos {
                let atlas = textures.get_mut(id).unwrap();
                for row in 0..image.height() {
                    let start = (y + row) * atlas.width() + x;
                    atlas.pixels[start..start + image.width()].copy_from_slice(
                        &image.pixels[row * image.width()..(row + 1) * image.width()],
                    );
                }
            } else {
                textures.insert(*id, (**image).clone());
            }
        }
    }
    let frame = frames.last().unwrap();
    let scale = frame.pixels_per_point;
    let mut image = image::RgbaImage::from_pixel(
        (1250.0 * scale) as u32,
        (730.0 * scale) as u32,
        image::Rgba([24, 24, 24, 255]),
    );
    let cross = |a: egui::Vec2, b: egui::Vec2| a.x * b.y - a.y * b.x;
    for primitive in ctx.tessellate(frame.shapes.clone(), scale) {
        let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive else {
            continue;
        };
        let Some(texture) = textures.get(&mesh.texture_id) else {
            continue;
        };
        let clip = primitive.clip_rect * scale;
        for indices in mesh.indices.chunks_exact(3) {
            let vertices = [
                mesh.vertices[indices[0] as usize],
                mesh.vertices[indices[1] as usize],
                mesh.vertices[indices[2] as usize],
            ];
            let p = vertices.map(|v| v.pos * scale);
            let area = cross(p[1] - p[0], p[2] - p[0]);
            if area.abs() < 0.0001 {
                continue;
            }
            let bounds = egui::Rect::from_min_max(
                egui::pos2(
                    p.iter().map(|p| p.x).fold(f32::INFINITY, f32::min),
                    p.iter().map(|p| p.y).fold(f32::INFINITY, f32::min),
                ),
                egui::pos2(
                    p.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max),
                    p.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max),
                ),
            )
            .intersect(clip);
            for y in (bounds.top().floor().max(0.0) as u32)
                ..(bounds.bottom().ceil().min(image.height() as f32) as u32)
            {
                for x in (bounds.left().floor().max(0.0) as u32)
                    ..(bounds.right().ceil().min(image.width() as f32) as u32)
                {
                    let point = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
                    let a = cross(p[1] - point, p[2] - point) / area;
                    let b = cross(p[2] - point, p[0] - point) / area;
                    let c = 1.0 - a - b;
                    if a < 0.0 || b < 0.0 || c < 0.0 {
                        continue;
                    }
                    let weights = [a, b, c];
                    let uv = vertices[0].uv.to_vec2() * a
                        + vertices[1].uv.to_vec2() * b
                        + vertices[2].uv.to_vec2() * c;
                    let u = (uv.x * texture.width() as f32 - 0.5)
                        .clamp(0.0, texture.width() as f32 - 1.0);
                    let v = (uv.y * texture.height() as f32 - 0.5)
                        .clamp(0.0, texture.height() as f32 - 1.0);
                    let tx = u.floor() as usize;
                    let ty = v.floor() as usize;
                    let dx = u.fract();
                    let dy = v.fract();
                    let sample = |xx: usize, yy: usize| {
                        texture.pixels[yy.min(texture.height() - 1) * texture.width()
                            + xx.min(texture.width() - 1)]
                        .to_array()
                    };
                    let samples = [
                        sample(tx, ty),
                        sample(tx + 1, ty),
                        sample(tx, ty + 1),
                        sample(tx + 1, ty + 1),
                    ];
                    let mut source = [0.0; 4];
                    for channel in 0..4 {
                        let tint: f32 = (0..3)
                            .map(|i| vertices[i].color.to_array()[channel] as f32 * weights[i])
                            .sum();
                        let value = samples[0][channel] as f32 * (1.0 - dx) * (1.0 - dy)
                            + samples[1][channel] as f32 * dx * (1.0 - dy)
                            + samples[2][channel] as f32 * (1.0 - dx) * dy
                            + samples[3][channel] as f32 * dx * dy;
                        source[channel] = tint * value / 255.0;
                    }
                    let destination = image.get_pixel_mut(x, y);
                    for channel in 0..3 {
                        destination[channel] = (source[channel]
                            + destination[channel] as f32 * (1.0 - source[3] / 255.0))
                            .clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }
    // The harness reserves the same space as the app's two navigation panels.
    image::imageops::crop_imm(
        &image,
        (359.0 * scale) as u32,
        0,
        (891.0 * scale) as u32,
        image.height(),
    )
    .to_image()
    .save(output)
    .unwrap();
}
