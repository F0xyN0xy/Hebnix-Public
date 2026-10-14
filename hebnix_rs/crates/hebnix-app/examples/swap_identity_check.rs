#![allow(dead_code, unused_imports)]
#[path = "../src/patcher/cosmetic_upk.rs"]
pub mod cosmetic_upk;
#[path = "../src/patcher/patch_core.rs"]
pub mod patch_core;
#[path = "../src/patcher/upk_keys.rs"]
pub mod upk_keys;
#[path = "../src/patcher/upk_package.rs"]
pub mod upk_package;
pub mod patcher {
    pub use crate::{patch_core, upk_package};
}
use serde_json::Value;
use std::path::Path;
fn collect<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(m) => {
            if m.contains_key("upk_path") && m.contains_key("path") {
                out.push(v);
            } else {
                for child in m.values() {
                    collect(child, out);
                }
            }
        }
        Value::Array(a) => {
            for child in a {
                collect(child, out);
            }
        }
        _ => {}
    }
}
fn check(
    root: &Path,
    output: &Path,
    donor: &str,
    target: &str,
    donor_asset: Option<&str>,
    target_asset: Option<&str>,
) -> Result<(), String> {
    let dest = output.join(target);
    // Exercise pristine donors with the backup filename used by the swapper.
    let backup = output.join(format!("{donor}.bak"));
    let pristine = |file: &str| {
        let backup = root.join("Backups").join(format!("{file}.bak"));
        if backup.is_file() {
            backup
        } else {
            root.join(file)
        }
    };
    std::fs::copy(pristine(donor), &backup).map_err(|e| e.to_string())?;
    cosmetic_upk::patch_for_target(
        &backup,
        &pristine(target),
        &dest,
        output,
        donor_asset,
        target_asset,
    )
    .map_err(|error| format!("{donor} -> {target}: {error}"))?;
    let source = upk_package::UpkPackage::load(&backup)?;
    let patched = upk_package::UpkPackage::load(&dest)?;
    assert_eq!(source.exports.len(), patched.exports.len());
    let delta = i64::from(patched.read_int(49)?) - i64::from(source.read_int(49)?);
    for (before, after) in source.exports.iter().zip(&patched.exports) {
        assert_eq!(
            (
                before.class_index,
                before.outer_index,
                before.serial_size,
                before.object_name.instance
            ),
            (
                after.class_index,
                after.outer_index,
                after.serial_size,
                after.object_name.instance
            )
        );
        if before.serial_size > 0 {
            assert_eq!(
                after.serial_offset as i64,
                before.serial_offset as i64 + delta
            );
            assert_eq!(
                source.image[before.serial_offset..before.serial_offset + before.serial_size],
                patched.image[after.serial_offset..after.serial_offset + after.serial_size]
            );
        }
    }
    if let Some(asset) = target_asset {
        let leaf = asset.rsplit('.').next().unwrap();
        assert!(patched.exports.iter().any(|e| {
            patched.names[e.object_name.name_index].trim_end_matches('\0') == leaf
                && !patched.class_of(e).starts_with("Package")
        }));
    }
    Ok(())
}
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--inspect") {
        for file in args.iter().skip(2) {
            let p = upk_package::UpkPackage::load(Path::new(file)).unwrap();
            println!("{file}");
            for e in &p.exports {
                let class = p.class_of(e);
                if class.contains("Product") || class.contains("Texture2D") {
                    println!(
                        "{} {} {}",
                        e.table_index,
                        class,
                        p.object_path(e.table_index as i32 + 1)
                    );
                }
            }
        }
        return;
    }
    let root = Path::new(args.get(1).expect("Provide CookedPCConsole directory"));
    let catalogs = Path::new(args.get(2).expect("Provide catalog directory"));
    let output = Path::new("target/swap-identity-check");
    std::fs::create_dir_all(output).unwrap();
    let mut passed = 0;
    let mut failed = 0;
    for category in [
        "antennas", "anthems", "borders", "bodies", "boosts", "skins", "engines", "goals",
        "finishes", "banners", "toppers", "trails", "wheels",
    ]
    .into_iter()
    .filter(|_| !args.iter().any(|a| a == "--targeted"))
    {
        let data: Value = serde_json::from_slice(
            &std::fs::read(catalogs.join(format!("{category}.json"))).unwrap(),
        )
        .unwrap();
        let mut entries = Vec::new();
        collect(&data, &mut entries);
        let mut items = Vec::new();
        for item in entries {
            let file = item["upk_path"].as_str().unwrap();
            if root.join(file).is_file()
                && !items
                    .iter()
                    .any(|v: &&Value| v["upk_path"] == item["upk_path"])
            {
                if category == "goals" && item["upk_type"].as_str().is_some_and(|t| t != "simple") {
                    continue;
                }
                items.push(item);
            }
            if items.len() == 3 {
                break;
            }
        }
        let mut checked = 0;
        for source in &items {
            for target in &items {
                if source["upk_path"] == target["upk_path"] {
                    continue;
                }
                let donor = source["upk_path"].as_str().unwrap();
                let target_file = target["upk_path"].as_str().unwrap();
                match check(
                    root,
                    output,
                    donor,
                    target_file,
                    source["path"].as_str(),
                    target["path"].as_str(),
                ) {
                    Ok(()) => {
                        passed += 1;
                        checked += 1;
                    }
                    Err(error) => {
                        failed += 1;
                        println!("FAIL {category} {donor} -> {target_file}: {error}");
                    }
                }
            }
        }
        println!("{category}: {checked} validated swaps");
    }
    for (donor, target, source_path, target_path) in [
        (
            "Boost_AlphaReward_SF.upk",
            "Boost_Standard_Yellow_SF.upk",
            "Boost_AlphaReward.Boost_AlphaReward",
            "Boost_Standard_Yellow.Boost_Standard_Yellow",
        ),
        (
            "ss_Lightning_SF.upk",
            "ss_binary_SF.upk",
            "ss_Lightning.ss_Lightning",
            "ss_binary.ss_binary",
        ),
        (
            "ss_binary_SF.upk",
            "ss_Lightning_SF.upk",
            "ss_binary.ss_binary",
            "ss_Lightning.ss_Lightning",
        ),
        (
            "anthem_teminiteandskybreak_accelerate_SF.upk",
            "mx_anthems_kk_SF.upk",
            "anthem_teminiteandskybreak_accelerate.anthem_teminiteandskybreak_accelerate",
            "mx_anthems_kk.Anthem_KK_ID1",
        ),
        (
            "mx_anthems_kk_SF.upk",
            "anthem_projectclyde_02_SF.upk",
            "mx_anthems_kk.Anthem_KK_ID1",
            "anthem_projectclyde_02.anthem_projectclyde_02",
        ),
    ] {
        check(
            root,
            output,
            donor,
            target,
            Some(source_path),
            Some(target_path),
        )
        .unwrap();
        passed += 1;
    }
    for (category, target_file) in [
        ("boosts", "Boost_Standard_Yellow_SF.upk"),
        ("trails", "ss_binary_SF.upk"),
    ] {
        let data: Value = serde_json::from_slice(
            &std::fs::read(catalogs.join(format!("{category}.json"))).unwrap(),
        )
        .unwrap();
        let mut entries = Vec::new();
        collect(&data, &mut entries);
        let target = entries
            .iter()
            .find(|v| {
                v["upk_path"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(target_file))
            })
            .unwrap();
        for source in entries
            .iter()
            .filter(|v| {
                v["upk_path"]
                    .as_str()
                    .is_some_and(|s| s != target_file && root.join(s).is_file())
            })
            .take(8)
        {
            check(
                root,
                output,
                source["upk_path"].as_str().unwrap(),
                target_file,
                source["path"].as_str(),
                target["path"].as_str(),
            )
            .unwrap();
            passed += 1;
            if let (Some(donor), Some(target_thumb)) =
                (source["thumbnail"].as_str(), target["thumbnail"].as_str())
            {
                if root.join(donor).is_file() && root.join(target_thumb).is_file() {
                    let pristine = |name: &str| {
                        let backup = root.join("Backups").join(format!("{name}.bak"));
                        if backup.is_file() {
                            backup
                        } else {
                            root.join(name)
                        }
                    };
                    let donor_asset = cosmetic_upk::thumbnail_asset_path(
                        &pristine(donor),
                        output,
                        source["path"].as_str(),
                    )
                    .unwrap();
                    let target_asset = cosmetic_upk::thumbnail_asset_path(
                        &pristine(target_thumb),
                        output,
                        target["path"].as_str(),
                    )
                    .unwrap();
                    check(
                        root,
                        output,
                        donor,
                        target_thumb,
                        Some(&donor_asset),
                        Some(&target_asset),
                    )
                    .unwrap();
                    passed += 1;
                }
            }
        }
    }
    println!("{passed} passed, {failed} failed");
    assert_eq!(failed, 0);
}
