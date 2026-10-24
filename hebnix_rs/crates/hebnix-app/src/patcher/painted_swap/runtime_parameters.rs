//! Persist colours at the parameter sources used when a model's materials are
//! rebuilt. A MIC default alone is discarded when the player equips a decal.
use super::{SwapPaint, fname, local_colour_parameter, parameter_name, property, struct_array};
use crate::patcher::upk_package::{Prop, UpkPackage, strip};
use std::{collections::HashSet, path::Path};

const NAMES: &[&str] = &[
    "SkinParameters",
    "MaterialParams",
    "VectorParameters",
    "Name",
    "Value",
    "NameProperty",
    "StructProperty",
    "ArrayProperty",
    "LinearColor",
    "TrimColor",
];

fn absolute(props: &mut [Prop], base: usize) {
    for prop in props {
        prop.tag_offset += base;
        prop.value_offset += base;
    }
}

fn replace_ranges(
    package: &UpkPackage,
    start: usize,
    end: usize,
    mut edits: Vec<(usize, usize, Vec<u8>)>,
) -> Vec<u8> {
    let mut bytes = package.image[start..end].to_vec();
    edits.sort_by_key(|edit| edit.0);
    for (from, to, value) in edits.into_iter().rev() {
        bytes.splice(from - start..to - start, value);
    }
    bytes
}

fn tag_with_value(package: &UpkPackage, prop: &Prop, value: &[u8]) -> Vec<u8> {
    let mut tag = package.image[prop.tag_offset..prop.value_offset].to_vec();
    tag[16..20].copy_from_slice(&(value.len() as i32).to_le_bytes());
    tag.extend(value);
    tag
}

fn vector_entry(package: &UpkPackage, name: &str, colour: &[u8]) -> Result<Vec<u8>, String> {
    let mut entry = property(
        package,
        "Name",
        "NameProperty",
        None,
        &fname(package, name)?,
    )?;
    entry.extend(property(
        package,
        "Value",
        "StructProperty",
        Some("LinearColor"),
        colour,
    )?);
    entry.extend(fname(package, "None")?);
    Ok(entry)
}

fn material_params(
    package: &UpkPackage,
    start: usize,
    end: usize,
    parameters: &HashSet<String>,
    colour: &[u8],
) -> Result<(Vec<u8>, usize), String> {
    let (props, parsed_end) = package.nested_props(start, end)?;
    if parsed_end != end {
        return Err("Unexpected material parameter tail".into());
    }
    let mut pending = parameters.clone();
    let mut changes = 0;
    let mut edits = Vec::new();
    if let Some(array) = props
        .iter()
        .find(|p| p.name == "VectorParameters" && p.tag_type == "ArrayProperty")
    {
        let mut value = package.image[array.value_offset..array.value_offset + 4].to_vec();
        let mut position = array.value_offset + 4;
        for fields in struct_array(package, array, 0)? {
            let (_, next) = package.nested_props(position, array.value_offset + array.size)?;
            let mut entry_edits = Vec::new();
            if let Some(name) = fields
                .iter()
                .find(|p| p.name == "Name" && p.tag_type == "NameProperty")
            {
                let name = parameter_name(package, name, 0)?;
                if parameters.contains(&name)
                    || (parameters.is_empty() && local_colour_parameter(&name))
                {
                    if let Some(target) = fields.iter().find(|p| p.name == "Value") {
                        if target.struct_name != "LinearColor" || target.size != 16 {
                            return Err("Invalid runtime material colour".into());
                        }
                        entry_edits.push((
                            target.value_offset,
                            target.value_offset + 16,
                            colour.to_vec(),
                        ));
                    } else {
                        entry_edits.push((
                            next - 8,
                            next - 8,
                            property(
                                package,
                                "Value",
                                "StructProperty",
                                Some("LinearColor"),
                                colour,
                            )?,
                        ));
                    }
                    pending.remove(&name);
                    changes += 1;
                }
            }
            value.extend(replace_ranges(package, position, next, entry_edits));
            position = next;
        }
        let mut added = 0;
        let mut names: Vec<_> = pending.into_iter().collect();
        names.sort();
        for name in names {
            value.extend(vector_entry(package, &name, colour)?);
            added += 1;
        }
        let count = package.read_int(array.value_offset)? + added;
        value[..4].copy_from_slice(&count.to_le_bytes());
        changes += added as usize;
        if changes > 0 {
            edits.push((
                array.tag_offset,
                array.value_offset + array.size,
                tag_with_value(package, array, &value),
            ));
        }
    } else if !pending.is_empty() {
        let mut names: Vec<_> = pending.into_iter().collect();
        names.sort();
        let mut value = (names.len() as i32).to_le_bytes().to_vec();
        for name in &names {
            value.extend(vector_entry(package, name, colour)?);
        }
        edits.push((
            end - 8,
            end - 8,
            property(package, "VectorParameters", "ArrayProperty", None, &value)?,
        ));
        changes += names.len();
    }
    Ok((replace_ranges(package, start, end, edits), changes))
}

// Only schema-known containers are traversed; native mesh/texture payloads and
// arbitrary arrays are never interpreted as tagged properties.
fn rewrite(
    package: &UpkPackage,
    props: &[Prop],
    start: usize,
    end: usize,
    parameters: &HashSet<String>,
    colour: &[u8],
) -> Result<(Vec<u8>, usize), String> {
    let mut edits = Vec::new();
    let mut changes = 0;
    for prop in props {
        let from = prop.value_offset;
        let to = from + prop.size;
        let (value, count) =
            if prop.tag_type == "StructProperty" && prop.struct_name == "MaterialParams" {
                let names = if prop.name == "SkinParameters" {
                    HashSet::from(["TrimColor".to_string()])
                } else {
                    parameters.clone()
                };
                material_params(package, from, to, &names, colour)?
            } else if prop.tag_type == "StructProperty"
                && matches!(
                    prop.struct_name.as_str(),
                    "SkinBodySettings"
                        | "AssociativeMaterialParams"
                        | "ProductAttachment"
                        | "FXAttachment"
                )
            {
                let (fields, _) = package.nested_props(from, to)?;
                rewrite(package, &fields, from, to, parameters, colour)?
            } else if prop.tag_type == "ArrayProperty"
                && matches!(
                    prop.name.as_str(),
                    "Attachments" | "BodySettings" | "AdditionalBodyParameters"
                )
            {
                let entries = struct_array(package, prop, 0)?;
                let mut value = package.image[from..from + 4].to_vec();
                let mut position = from + 4;
                let mut count = 0;
                for fields in entries {
                    let (_, next) = package.nested_props(position, to)?;
                    let (entry, changed) =
                        rewrite(package, &fields, position, next, parameters, colour)?;
                    value.extend(entry);
                    count += changed;
                    position = next;
                }
                (value, count)
            } else {
                continue;
            };
        if count > 0 {
            edits.push((prop.tag_offset, to, tag_with_value(package, prop, &value)));
            changes += count;
        }
    }
    Ok((replace_ranges(package, start, end, edits), changes))
}

pub(super) fn bake(path: &Path, paint: SwapPaint, write: bool) -> Result<usize, String> {
    let mut package = UpkPackage::load(path)?;
    let missing = NAMES
        .iter()
        .any(|name| !package.names.iter().any(|n| n == name));
    for name in NAMES {
        if !package.names.iter().any(|n| n == name) {
            package.names.push((*name).into());
        }
    }
    let mut parameters = HashSet::new();
    for export in &package.exports {
        if strip(&package.class_of(export)) == "ProductAttribute_PaintSettings_TA" {
            let (props, _) = package.serialized_props(export)?;
            parameters.insert(
                if let Some(name) = props
                    .iter()
                    .find(|p| p.name == "PaintParameterName" && p.tag_type == "NameProperty")
                {
                    parameter_name(&package, name, export.serial_offset)?
                } else {
                    "CustomColor".into()
                },
            );
        }
    }
    let colour: Vec<_> = paint
        .material()
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let mut updates = Vec::new();
    let mut count = 0;
    for export in &package.exports {
        let class = package.class_of(export);
        if !strip(&class).starts_with("ProductAsset_")
            && !matches!(
                strip(&class),
                "AssetAttribute_TeamParameterOverride_TA" | "FXActor_TA" | "FXActor_Boost_TA"
            )
        {
            continue;
        }
        let (mut props, native) = package.serialized_props(export)?;
        absolute(&mut props, export.serial_offset);
        let (mut bytes, mut changed) = rewrite(
            &package,
            &props,
            export.serial_offset,
            export.serial_offset + export.serial_size,
            &parameters,
            &colour,
        )?;
        if strip(&class) == "ProductAsset_Body_TA"
            && !props.iter().any(|p| p.name == "SkinParameters")
        {
            let value = vector_entry(&package, "TrimColor", &colour)?;
            let mut array = 1i32.to_le_bytes().to_vec();
            array.extend(value);
            let mut fields = property(&package, "VectorParameters", "ArrayProperty", None, &array)?;
            fields.extend(fname(&package, "None")?);
            let tag = property(
                &package,
                "SkinParameters",
                "StructProperty",
                Some("MaterialParams"),
                &fields,
            )?;
            let insertion = bytes.len() - (export.serial_size - native) - 8;
            bytes.splice(insertion..insertion, tag);
            changed += 1;
        }
        if changed > 0 {
            updates.push((export.table_index, bytes));
            count += changed;
        }
    }
    if count == 0 || !write {
        return Ok(count);
    }
    if missing {
        let prepared = path.with_extension("runtime-names.tmp");
        let result = (|| {
            crate::cosmetic_upk::append_names(path, &prepared, NAMES)?;
            let count = bake(&prepared, paint, true)?;
            std::fs::copy(&prepared, path).map_err(|e| e.to_string())?;
            Ok(count)
        })();
        let _ = std::fs::remove_file(prepared);
        return result;
    }
    for (index, bytes) in &updates {
        package.replace_export_payload(*index, bytes)?;
    }
    package.save(path)?;
    let check = UpkPackage::load(path)?;
    for (index, bytes) in updates {
        let export = &check.exports[index];
        if check
            .image
            .get(export.serial_offset..export.serial_offset + export.serial_size)
            != Some(bytes.as_slice())
        {
            return Err("Runtime recolour verification failed".into());
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    fn fixture(name: &str) -> Option<(PathBuf, PathBuf)> {
        let game = PathBuf::from(std::env::var_os("HEBNIX_RECOLOUR_GAME_DIR")?);
        let root =
            std::env::temp_dir().join(format!("hebnix-runtime-paint-{}", rand::random::<u64>()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join(name);
        let backup = game.join("Backups").join(format!("{name}.bak"));
        let source = if backup.is_file() {
            backup
        } else {
            game.join(name)
        };
        crate::cosmetic_upk::append_names(&source, &path, NAMES).unwrap();
        Some((root, path))
    }

    fn set_body_parameters(package: &mut UpkPackage, fields: Option<&[u8]>) {
        for export in package
            .exports
            .clone()
            .into_iter()
            .filter(|e| strip(&package.class_of(e)) == "ProductAsset_Body_TA")
            .collect::<Vec<_>>()
        {
            let (props, native) = package.serialized_props(&export).unwrap();
            let mut bytes = package.image
                [export.serial_offset..export.serial_offset + export.serial_size]
                .to_vec();
            let tag = fields
                .map(|fields| {
                    property(
                        package,
                        "SkinParameters",
                        "StructProperty",
                        Some("MaterialParams"),
                        fields,
                    )
                    .unwrap()
                })
                .unwrap_or_default();
            if let Some(skin) = props.iter().find(|p| p.name == "SkinParameters") {
                bytes.splice(skin.tag_offset..skin.value_offset + skin.size, tag);
            } else {
                bytes.splice(native - 8..native - 8, tag);
            }
            package
                .replace_export_payload(export.table_index, &bytes)
                .unwrap();
        }
    }

    fn check_body_colour(path: &Path, paint: SwapPaint) {
        let package = UpkPackage::load(path).unwrap();
        for export in &package.exports {
            if strip(&package.class_of(export)) != "ProductAsset_Body_TA" {
                continue;
            }
            let (props, _) = package.serialized_props(export).unwrap();
            let skins: Vec<_> = props
                .iter()
                .filter(|p| p.name == "SkinParameters")
                .collect();
            assert_eq!(skins.len(), 1);
            let skin = skins[0];
            let start = export.serial_offset + skin.value_offset;
            let (props, _) = package.nested_props(start, start + skin.size).unwrap();
            let array = props.iter().find(|p| p.name == "VectorParameters").unwrap();
            let entries = struct_array(&package, array, 0).unwrap();
            let trim: Vec<_> = entries
                .iter()
                .filter(|fields| {
                    fields.iter().any(|p| {
                        p.name == "Name" && parameter_name(&package, p, 0).unwrap() == "TrimColor"
                    })
                })
                .collect();
            assert_eq!(trim.len(), 1);
            let value = trim[0].iter().find(|p| p.name == "Value").unwrap();
            let colour: Vec<_> = paint
                .material()
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            assert_eq!(
                &package.image[value.value_offset..value.value_offset + value.size],
                colour
            );
        }
    }

    #[test]
    fn omitted_model_parameters_are_created_and_survive_repacking() {
        let Some((root, path)) = fixture("body_grain_SF.upk") else {
            return;
        };
        let mut package = UpkPackage::load(&path).unwrap();
        set_body_parameters(&mut package, None);
        package.save(&path).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(bake(&path, SwapPaint::Crimson, false).unwrap() > 0);
        assert_eq!(fs::read(&path).unwrap(), before);
        bake(&path, SwapPaint::Crimson, true).unwrap();
        check_body_colour(&path, SwapPaint::Crimson);
        bake(&path, SwapPaint::Custom([32, 128, 240]), true).unwrap();
        check_body_colour(&path, SwapPaint::Custom([32, 128, 240]));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn omitted_runtime_colour_values_are_created_without_duplicate_entries() {
        let Some((root, path)) = fixture("body_grain_SF.upk") else {
            return;
        };
        let mut package = UpkPackage::load(&path).unwrap();
        let mut entry = property(
            &package,
            "Name",
            "NameProperty",
            None,
            &fname(&package, "TrimColor").unwrap(),
        )
        .unwrap();
        entry.extend(fname(&package, "None").unwrap());
        let mut array = 1i32.to_le_bytes().to_vec();
        array.extend(entry);
        let mut fields =
            property(&package, "VectorParameters", "ArrayProperty", None, &array).unwrap();
        fields.extend(fname(&package, "None").unwrap());
        set_body_parameters(&mut package, Some(&fields));
        package.save(&path).unwrap();
        bake(&path, SwapPaint::TitaniumWhite, true).unwrap();
        check_body_colour(&path, SwapPaint::TitaniumWhite);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_runtime_parameter_preserves_the_entire_previous_paint() {
        let Some((root, path)) = fixture("body_grain_SF.upk") else {
            return;
        };
        let mut package = UpkPackage::load(&path).unwrap();
        let mut entry = property(
            &package,
            "Name",
            "NameProperty",
            None,
            &fname(&package, "TrimColor").unwrap(),
        )
        .unwrap();
        // A valid tag with the wrong value type must fail before installation.
        entry.extend(
            property(
                &package,
                "Value",
                "NameProperty",
                None,
                &fname(&package, "None").unwrap(),
            )
            .unwrap(),
        );
        entry.extend(fname(&package, "None").unwrap());
        let mut array = 1i32.to_le_bytes().to_vec();
        array.extend(entry);
        let mut fields =
            property(&package, "VectorParameters", "ArrayProperty", None, &array).unwrap();
        fields.extend(fname(&package, "None").unwrap());
        set_body_parameters(&mut package, Some(&fields));
        package.save(&path).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(super::super::bake(&path, "body_grain_SF.upk", SwapPaint::Crimson).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_decal_material_parameters_update_parent_sizes_and_preserve_other_fields() {
        let Some((root, path)) = fixture("skin_zomba_SF.upk") else {
            return;
        };
        crate::cosmetic_upk::append_names(
            &path,
            &root.join("names.upk"),
            &["BodySettings", "Parameters", "TeamColor"],
        )
        .unwrap();
        let mut package = UpkPackage::load(&root.join("names.upk")).unwrap();
        let export = package
            .exports
            .iter()
            .find(|e| strip(&package.class_of(e)) == "ProductAsset_Skin_TA")
            .unwrap()
            .clone();
        let (props, native) = package.serialized_props(&export).unwrap();
        let original =
            package.image[export.serial_offset..export.serial_offset + export.serial_size].to_vec();
        let team_colour = [0.25f32, 0.5, 0.75, 1.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        let mut vectors = 1i32.to_le_bytes().to_vec();
        vectors.extend(vector_entry(&package, "TeamColor", &team_colour).unwrap());
        let mut params = property(
            &package,
            "VectorParameters",
            "ArrayProperty",
            None,
            &vectors,
        )
        .unwrap();
        params.extend(fname(&package, "None").unwrap());
        let mut entry = property(
            &package,
            "Parameters",
            "StructProperty",
            Some("MaterialParams"),
            &params,
        )
        .unwrap();
        entry.extend(fname(&package, "None").unwrap());
        let mut array = 1i32.to_le_bytes().to_vec();
        array.extend(entry);
        let tag = property(&package, "BodySettings", "ArrayProperty", None, &array).unwrap();
        let mut bytes = original;
        if let Some(prop) = props.iter().find(|p| p.name == "BodySettings") {
            bytes.splice(prop.tag_offset..prop.value_offset + prop.size, tag);
        } else {
            bytes.splice(native - 8..native - 8, tag);
        }
        package
            .replace_export_payload(export.table_index, &bytes)
            .unwrap();
        package.save(&path).unwrap();
        bake(&path, SwapPaint::Crimson, true).unwrap();
        let check = UpkPackage::load(&path).unwrap();
        let export = &check.exports[export.table_index];
        let (props, _) = check.serialized_props(export).unwrap();
        let array = props.iter().find(|p| p.name == "BodySettings").unwrap();
        let entries = struct_array(&check, array, export.serial_offset).unwrap();
        let params = entries[0].iter().find(|p| p.name == "Parameters").unwrap();
        let (fields, _) = check
            .nested_props(params.value_offset, params.value_offset + params.size)
            .unwrap();
        let vectors = fields
            .iter()
            .find(|p| p.name == "VectorParameters")
            .unwrap();
        let entries = struct_array(&check, vectors, 0).unwrap();
        assert_eq!(entries.len(), 2);
        for fields in entries {
            let name = parameter_name(&check, fields.iter().find(|p| p.name == "Name").unwrap(), 0)
                .unwrap();
            let value = fields.iter().find(|p| p.name == "Value").unwrap();
            let expected = if name == "TeamColor" {
                team_colour.clone()
            } else {
                assert_eq!(name, "PaintColor");
                SwapPaint::Crimson
                    .material()
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect()
            };
            assert_eq!(
                &check.image[value.value_offset..value.value_offset + value.size],
                expected
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
