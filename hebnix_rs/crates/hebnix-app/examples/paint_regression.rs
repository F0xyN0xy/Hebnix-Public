//! Offline integration checks against pristine installed assets; writes only target/.
//! cargo run -p hebnix-app --example paint_regression --offline -- <CookedPCConsole> <catalogs>
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
use painted_swap::SwapPaint;
use std::path::{Path, PathBuf};
use upk_package::{UpkPackage, strip};

fn pristine(root: &Path, name: &str) -> PathBuf {
    let backup = root.join("Backups").join(format!("{name}.bak"));
    if backup.is_file() {
        backup
    } else {
        root.join(name)
    }
}
fn collect<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a serde_json::Value>) {
    if v.get("upk_path").is_some() {
        out.push(v);
        return;
    }
    match v {
        serde_json::Value::Object(map) => {
            for c in map.values() {
                collect(c, out);
            }
        }
        serde_json::Value::Array(a) => {
            for c in a {
                collect(c, out);
            }
        }
        _ => {}
    }
}
fn linear(rgb: [u8; 3]) -> [f32; 3] {
    rgb.map(|v| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    })
}
fn colour_at(p: &UpkPackage, at: usize) -> [f32; 3] {
    std::array::from_fn(|i| {
        f32::from_le_bytes(p.image[at + i * 4..at + i * 4 + 4].try_into().unwrap())
    })
}
fn verify(before: &UpkPackage, after: &UpkPackage, rgb: [u8; 3]) {
    assert_eq!(before.exports.len(), after.exports.len());
    let expected = linear(rgb);
    let mut painted = 0;
    for (b, a) in before.exports.iter().zip(&after.exports) {
        let class = strip(&before.class_of(b)).to_owned();
        assert_eq!(b.class_index, a.class_index);
        assert_eq!(b.outer_index, a.outer_index);
        if class == "DistributionVectorParticleParameter" {
            let parse = |p: &UpkPackage, e: &upk_package::ExportEntry| {
                p.nested_props(
                    e.serial_offset
                        + if p.read_int(e.serial_offset + 4).unwrap() == -1 {
                            8
                        } else {
                            4
                        },
                    e.serial_offset + e.serial_size,
                )
                .unwrap()
                .0
            };
            let old = parse(before, b);
            let new = parse(after, a);
            if let Some(name) = new.iter().find(|p| p.name == "ParameterName") {
                let name_id = after.read_int(name.value_offset).unwrap() as usize;
                let old_name = old.iter().find(|p| p.name == "ParameterName").unwrap();
                let old_id = before.read_int(old_name.value_offset).unwrap() as usize;
                if after.names[name_id] == "None" && before.names[old_id] != "None" {
                    let constant = new
                        .iter()
                        .find(|p| p.name == "Constant")
                        .expect("frozen particle needs explicit colour");
                    let peak = old
                        .iter()
                        .find(|p| p.name == "Constant")
                        .map(|c| {
                            colour_at(before, c.value_offset)
                                .into_iter()
                                .fold(0.0f32, f32::max)
                        })
                        .filter(|v| *v > 0.0)
                        .unwrap_or(1.0);
                    for (got, want) in colour_at(after, constant.value_offset)
                        .into_iter()
                        .zip(expected.map(|v| v * peak))
                    {
                        assert!(
                            (got - want).abs() < 0.00001 * want.abs().max(1.0),
                            "{class}: {got} != {want}"
                        );
                    }
                    painted += 1;
                }
            }
        } else if matches!(
            class.as_str(),
            "MaterialInstanceConstant" | "ProductOverride_MaterialParameter_TA"
        ) {
            let vectors = |p: &UpkPackage, e: &upk_package::ExportEntry| {
                let (props, _) = p.serialized_props(e).unwrap();
                let mut values = std::collections::HashMap::new();
                for array in props.iter().filter(|p| p.name == "VectorParameterValues") {
                    let mut pos = e.serial_offset + array.value_offset + 4;
                    let end = e.serial_offset + array.value_offset + array.size;
                    for _ in 0..p.read_int(pos - 4).unwrap() {
                        let (fields, next) = p.nested_props(pos, end).unwrap();
                        let name = fields.iter().find(|f| f.name == "ParameterName").unwrap();
                        let value = fields.iter().find(|f| f.name == "ParameterValue").unwrap();
                        let name = p.names[p.read_int(name.value_offset).unwrap() as usize].clone();
                        let occurrence = values
                            .keys()
                            .filter(|(existing, _)| existing == &name)
                            .count();
                        values.insert((name, occurrence), colour_at(p, value.value_offset));
                        pos = next;
                    }
                    assert_eq!(pos, end);
                }
                values
            };
            let old = vectors(before, b);
            for (name, value) in vectors(after, a) {
                if old.get(&name) != Some(&value) {
                    assert_eq!(value, expected, "{class} {name:?}");
                    painted += 1;
                }
            }
        } else if class.starts_with("ProductAsset_") {
            let (props, _) = after.serialized_props(a).unwrap();
            for colour in props.iter().filter(|p| p.name == "DefaultPaintColor") {
                assert_eq!(
                    colour_at(after, a.serial_offset + colour.value_offset),
                    expected
                );
                painted += 1;
            }
            for painted_prop in props
                .iter()
                .filter(|p| p.name.starts_with("Painted") && p.tag_type == "ObjectProperty")
            {
                if let Some(normal) = props.iter().find(|p| {
                    p.name == painted_prop.name.trim_start_matches("Painted")
                        && p.tag_type == "ObjectProperty"
                }) {
                    let value = after
                        .read_int(a.serial_offset + painted_prop.value_offset)
                        .unwrap();
                    if value > 0 {
                        assert_eq!(
                            after
                                .read_int(a.serial_offset + normal.value_offset)
                                .unwrap(),
                            value
                        );
                    }
                }
            }
        } else if !matches!(
            class.as_str(),
            "MaterialInstanceConstant"
                | "MaterialExpressionVectorParameter"
                | "DistributionFloatParticleParameter"
                | "Texture2D"
                | "TextureCube"
        ) {
            assert_eq!(
                &before.image[b.serial_offset..b.serial_offset + b.serial_size],
                &after.image[a.serial_offset..a.serial_offset + a.serial_size],
                "unexpected change to {class}"
            );
        }
    }
    if before
        .exports
        .iter()
        .any(|e| strip(&before.class_of(e)) == "ProductAsset_GoalExplosion_TA")
    {
        assert!(
            painted > 0,
            "goal must have frozen painted particle colours"
        );
    }
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let root = Path::new(args.get(1).expect("CookedPCConsole"));
    let catalogs = Path::new(args.get(2).expect("Catalog directory"));
    let output = Path::new("target/paint-regression");
    std::fs::create_dir_all(output).unwrap();
    let mut count = 0;
    for (category, files) in [
        (
            "goals",
            vec![
                "Explosion_Leaves_SF.upk",
                "explosion_missiles_SF.upk",
                "explosion_splash_SF.upk",
                "explosion_ioncannon_SF.upk",
                "explosion_dynamicenergy_SF.upk",
            ],
        ),
        (
            "bodies",
            vec![
                "Body_Octane_SF.upk",
                "body_grain_SF.upk",
                "body_ramen_SF.upk",
                "Body_Force_SF.upk",
                "Body_Torch_SF.upk",
                "Body_Aftershock_SF.upk",
            ],
        ),
        ("boosts", vec!["Boost_Standard_SF.upk"]),
        (
            "wheels",
            vec![
                "wheel_hiro_SF.upk",
                "wheel_reaper_SF.upk",
                "wheel_hanzawa_psplus_SF.upk",
            ],
        ),
        ("skins", vec!["skin_zomba_SF.upk"]),
        ("trails", vec!["ss_binary_SF.upk", "ss_equalizer_SF.upk"]),
        ("banners", vec!["PlayerBanner_Zombie_SF.upk"]),
        (
            "antennas",
            vec![
                "Antenna_Bomb_SF.upk",
                "antenna_golfdriver_SF.upk",
                "antenna_turbine_SF.upk",
            ],
        ),
        ("toppers", vec!["Hat_Crown_SF.upk", "Hat_Party_SF.upk"]),
        ("borders", vec!["avatarborder_braidedrope_SF.upk"]),
    ] {
        let data: serde_json::Value = serde_json::from_slice(
            &std::fs::read(catalogs.join(format!("{category}.json"))).unwrap(),
        )
        .unwrap();
        let mut items = Vec::new();
        collect(&data, &mut items);
        for file in files.into_iter().filter(|file| {
            args.get(3).is_none_or(|filter| {
                file.to_ascii_lowercase()
                    .contains(&filter.to_ascii_lowercase())
            })
        }) {
            let item = items
                .iter()
                .find(|item| {
                    item["upk_path"]
                        .as_str()
                        .is_some_and(|v| v.eq_ignore_ascii_case(file))
                })
                .expect(file);
            let target = if category == "goals" {
                *items
                    .iter()
                    .find(|item| item["upk_path"] == "Explosion_Default_SF.upk")
                    .unwrap()
            } else {
                *item
            };
            let target_file = target["upk_path"].as_str().unwrap();
            let dest = output.join(target_file);
            let identity = output.join("identity").join(target_file);
            std::fs::create_dir_all(identity.parent().unwrap()).unwrap();
            let source = pristine(root, file);
            let target_backup = pristine(root, target_file);
            let donor_path = if category == "goals" {
                item["path"].as_str()
            } else {
                None
            };
            let target_path = if category == "goals" {
                target["path"].as_str()
            } else {
                None
            };
            cosmetic_upk::patch_for_target(
                &source,
                &target_backup,
                &identity,
                root,
                donor_path,
                target_path,
            )
            .unwrap();
            let before = UpkPackage::load(&identity).unwrap();
            for rgb in if category == "goals" {
                vec![[0, 0, 0], [32, 128, 240], [255, 255, 255]]
            } else {
                vec![[32, 128, 240]]
            } {
                painted_swap::patch_for_target(
                    &source,
                    &target_backup,
                    &dest,
                    root,
                    donor_path,
                    target_path,
                    file,
                    SwapPaint::Custom(rgb),
                )
                .unwrap();
                let after = UpkPackage::load(&dest).unwrap();
                verify(&before, &after, rgb);
                count += 1;
                println!(
                    "PASS {file} -> {target_file} #{:02X}{:02X}{:02X}",
                    rgb[0], rgb[1], rgb[2]
                );
            }
            painted_swap::patch_for_target(
                &source,
                &target_backup,
                &dest,
                root,
                donor_path,
                target_path,
                file,
                SwapPaint::Default,
            )
            .unwrap();
            assert_eq!(
                std::fs::read(&dest).unwrap(),
                std::fs::read(&identity).unwrap()
            );
            let unchanged = std::fs::read(&dest).unwrap();
            assert!(
                painted_swap::patch_for_target(
                    &source,
                    &target_backup,
                    &dest,
                    root,
                    donor_path,
                    target_path,
                    file,
                    SwapPaint::Preset(0)
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&dest).unwrap(), unchanged);
        }
    }
    println!(
        "PASS {count} painted swaps; default restoration and failed-swap preservation passed for every donor"
    );
}
