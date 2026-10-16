#![allow(dead_code, unused_imports)]
#[path = "../src/patcher/cosmetic_upk.rs"]
pub mod cosmetic_upk;
#[path = "../src/patcher/painted_swap.rs"]
pub mod painted_swap;
#[path = "../src/patcher/patch_core.rs"]
pub mod patch_core;
#[path = "../src/patcher/upk_keys.rs"]
pub mod upk_keys;
#[path = "../src/patcher/upk_package.rs"]
pub mod upk_package;
pub mod patcher {
    pub use crate::{painted_swap, patch_core, upk_package};
}
use std::path::Path;
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = Path::new(args.get(1).expect("CookedPCConsole directory"));
    if args.get(2).is_some_and(|a| a == "--audit") {
        fn collect<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a serde_json::Value>) {
            if value.get("upk_path").is_some() {
                out.push(value);
                return;
            }
            match value {
                serde_json::Value::Object(map) => {
                    for child in map.values() {
                        collect(child, out);
                    }
                }
                serde_json::Value::Array(array) => {
                    for child in array {
                        collect(child, out);
                    }
                }
                _ => {}
            }
        }
        let catalogs = Path::new(args.get(3).expect("Catalog directory"));
        let mut total = 0;
        let mut failed = 0;
        for category in [
            "antennas", "anthems", "borders", "bodies", "boosts", "skins", "engines", "goals",
            "finishes", "banners", "toppers", "trails", "wheels",
        ] {
            let data: serde_json::Value = serde_json::from_slice(
                &std::fs::read(catalogs.join(format!("{category}.json"))).unwrap(),
            )
            .unwrap();
            let mut entries = Vec::new();
            collect(&data, &mut entries);
            let mut seen = std::collections::HashSet::new();
            let mut passed = 0;
            let mut errors = 0;
            for item in entries {
                let name = item["upk_path"].as_str().unwrap();
                if !(item["paintable"].as_bool() == Some(true) || painted_swap::supports(name))
                    || !seen.insert(name.to_ascii_lowercase())
                {
                    continue;
                }
                let backup = root.join("Backups").join(format!("{name}.bak"));
                let path = if backup.is_file() {
                    backup
                } else {
                    root.join(name)
                };
                if !path.is_file() {
                    continue;
                }
                match painted_swap::inspect_paint(
                    &path,
                    name,
                    painted_swap::SwapPaint::Custom([32, 128, 240]),
                ) {
                    Ok(_) => passed += 1,
                    Err(error) => {
                        errors += 1;
                        println!(
                            "FAIL {category} {} {name}: {error}",
                            item["name"].as_str().unwrap_or("")
                        );
                    }
                }
            }
            total += passed;
            failed += errors;
            println!("{category}: {passed} planned, {errors} failed");
        }
        println!("TOTAL {total} planned, {failed} failed");
        return;
    }
    for name in args.iter().skip(2) {
        let backup = root.join("Backups").join(format!("{name}.bak"));
        let path = if backup.is_file() {
            backup
        } else {
            root.join(name)
        };
        let p = upk_package::UpkPackage::load(&path).unwrap();
        println!("PACKAGE {name}");
        for e in &p.exports {
            let class = upk_package::strip(&p.class_of(e)).to_owned();
            if !(class.contains("PaintSettings")
                || class.contains("Painted")
                || class.starts_with("ProductOverride")
                || class == "MaterialInstanceConstant"
                || class == "MaterialExpressionVectorParameter"
                || class == "DistributionVectorParticleParameter"
                || class.starts_with("ProductAsset_")
                || class == "ParameterDispenser_X"
                || class == "FXActor_TA"
                || class.starts_with("ParticleSystemComponent"))
            {
                continue;
            }
            println!(
                "EXPORT {} {class} {}",
                e.table_index + 1,
                p.object_path(e.table_index as i32 + 1)
            );
            let props = if class == "DistributionVectorParticleParameter" {
                p.nested_props(e.serial_offset + 8, e.serial_offset + e.serial_size)
                    .map(|v| v.0)
            } else {
                p.serialized_props(e)
                    .map(|v| v.0)
                    .or_else(|_| Ok::<_, String>(p.parse_props(e)))
            };
            match props {
                Err(error) => println!("  ERROR {error}"),
                Ok(props) => {
                    for prop in props {
                        let at = if class == "DistributionVectorParticleParameter" {
                            prop.value_offset
                        } else {
                            e.serial_offset + prop.value_offset
                        };
                        let bytes = &p.image[at..at + prop.size];
                        let value = match prop.tag_type.as_str() {
                            "NameProperty" | "ByteProperty" => p
                                .names
                                .get(p.read_int(at).unwrap() as usize)
                                .cloned()
                                .unwrap_or_default(),
                            "ObjectProperty" => p.object_path(p.read_int(at).unwrap()),
                            _ => format!(
                                "{:?}",
                                bytes
                                    .chunks_exact(4)
                                    .take(24)
                                    .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
                                    .collect::<Vec<_>>()
                            ),
                        };
                        println!(
                            "  {} {} {} size={} at={} {value}",
                            prop.name, prop.tag_type, prop.struct_name, prop.size, at
                        );
                        if prop.tag_type == "ArrayProperty"
                            && matches!(
                                prop.name.as_str(),
                                "PaintAdditionalParameters"
                                    | "ParameterOverrides"
                                    | "VectorParameters"
                                    | "VectorParameterValues"
                                    | "ColorParameters"
                                    | "InstanceParameters"
                                    | "PaintsToOverride"
                            )
                        {
                            let mut pos = at + 4;
                            for _ in 0..p.read_int(at).unwrap().min(20) {
                                let Ok((fields, next)) = p.nested_props(pos, at + prop.size) else {
                                    break;
                                };
                                for f in fields {
                                    let val = if f.tag_type == "NameProperty"
                                        || f.tag_type == "ByteProperty"
                                    {
                                        p.names
                                            .get(p.read_int(f.value_offset).unwrap() as usize)
                                            .cloned()
                                            .unwrap_or_default()
                                    } else {
                                        format!(
                                            "{:?}",
                                            p.image[f.value_offset..f.value_offset + f.size]
                                                .chunks_exact(4)
                                                .take(20)
                                                .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
                                                .collect::<Vec<_>>()
                                        )
                                    };
                                    println!(
                                        "    {} {} {} at={} {val}",
                                        f.name, f.tag_type, f.struct_name, f.value_offset
                                    );
                                }
                                pos = next;
                            }
                        }
                    }
                }
            }
        }
        let out = Path::new("target/paint-check");
        std::fs::create_dir_all(out).unwrap();
        let staged = out.join(name);
        std::fs::copy(&path, &staged).unwrap();
        println!(
            "BAKE {:?}",
            painted_swap::bake(&staged, name, painted_swap::SwapPaint::Custom([0, 255, 0]))
        );
    }
}
