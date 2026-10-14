#![allow(dead_code, unused_imports)]
#[path = "../src/patcher/cosmetic_thumbnail.rs"]
pub mod cosmetic_thumbnail;
#[path = "../src/patcher/patch_core.rs"]
pub mod patch_core;
#[path = "../src/patcher/upk_keys.rs"]
pub mod upk_keys;
#[path = "../src/patcher/upk_package.rs"]
pub mod upk_package;
pub mod patcher {
    pub use crate::{patch_core, upk_package};
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = std::path::Path::new(args.get(1).expect("CookedPCConsole directory"));
    let output = std::path::Path::new("target/thumbnail-check");
    std::fs::create_dir_all(output).unwrap();
    if args.get(2).is_some_and(|a| a == "--engines") {
        let catalogs = std::path::Path::new(args.get(3).expect("Catalog directory"));
        let bodies: serde_json::Value =
            serde_json::from_slice(&std::fs::read(catalogs.join("bodies.json")).unwrap()).unwrap();
        let engines: serde_json::Value =
            serde_json::from_slice(&std::fs::read(catalogs.join("engines.json")).unwrap()).unwrap();
        let mut decoded = 0;
        let mut unavailable = 0;
        let mut body_previews = 0;
        for engine in engines["engines"].as_array().unwrap() {
            let name = engine["name"].as_str().unwrap();
            let upk = engine["upk_path"].as_str().unwrap();
            if !root.join(upk).is_file() {
                continue;
            }
            let body = cosmetic_thumbnail::body_preview_for_engine(name, upk, &bodies)
                .filter(|p| root.join(p).is_file());
            let Some(thumbnail) = body.as_deref().or_else(|| engine["thumbnail"].as_str()) else {
                continue;
            };
            match cosmetic_thumbnail::extract_png(&root.join(thumbnail), "engines") {
                Ok(png) => {
                    decoded += 1;
                    if body.is_some() {
                        body_previews += 1;
                    }
                    if name.contains("Aston Martin") {
                        std::fs::write(output.join(format!("{upk}.png")), &png).unwrap();
                        println!("PASS {name}: {thumbnail}");
                    }
                }
                Err(error) => {
                    unavailable += 1;
                    println!("FAIL {name}: {error}");
                }
            }
        }
        println!(
            "{decoded} decoded, {body_previews} associated car previews, {unavailable} unavailable"
        );
        return;
    }
    for name in args.iter().skip(2) {
        let path = root.join(name);
        match cosmetic_thumbnail::extract_png(&path, "") {
            Ok(png) => {
                std::fs::write(output.join(format!("{name}.png")), &png).unwrap();
                println!("PASS {name} ({} bytes)", png.len());
            }
            Err(error) => println!("FAIL {name}: {error}"),
        }
        if let Ok(p) = upk_package::UpkPackage::load(&path) {
            for e in &p.exports {
                let class = p.class_of(e);
                if class.contains("ProductThumbnailAsset") || class.contains("Texture2D") {
                    println!(
                        "  {} {} {}",
                        e.table_index + 1,
                        class,
                        p.object_path(e.table_index as i32 + 1)
                    );
                    if let Ok((props, native)) = p.serialized_props(e) {
                        for prop in props.iter().filter(|v| {
                            matches!(
                                v.name.as_str(),
                                "Thumbnail" | "TextureFileCacheName" | "Format" | "SizeX" | "SizeY"
                            )
                        }) {
                            let at = e.serial_offset + prop.value_offset;
                            println!(
                                "    {} {} {:?}",
                                prop.name,
                                prop.tag_type,
                                &p.image[at..at + prop.size]
                            );
                        }
                        println!("    native={native} size={}", e.serial_size);
                    }
                }
            }
            for (path, class, _) in p.import_debug().iter().filter(|(_, class, _)| {
                class.contains("Texture2D") || class.contains("ProductThumbnailAsset")
            }) {
                println!("  import {class} {path}");
            }
        }
    }
}
