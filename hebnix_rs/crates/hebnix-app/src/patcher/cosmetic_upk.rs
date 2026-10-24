use aes::Aes256;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use std::fs;
use std::path::Path;

const UPK_MAGIC: u32 = 2_653_586_369;
const DEFAULT_KEY: [u8; 32] = [
    199, 223, 107, 19, 37, 42, 204, 113, 71, 187, 81, 201, 138, 215, 227, 75, 127, 229, 0, 183,
    127, 165, 250, 178, 147, 226, 242, 78, 107, 23, 231, 121,
];

#[derive(Clone, Copy)]
struct UpkInfo {
    licensee_version: u16,
    package_flags: u32,
    nonce: [u8; 12],
    total_header_size: usize,
    name_count: usize,
    name_offset: usize,
    export_count: usize,
    export_offset: usize,
    import_count: usize,
    import_offset: usize,
    package_guid_offset: usize,
    generation_name_count_offset: Option<usize>,
    generation_size: i32,
    last_block_size: usize,
    generation_size_offset: usize,
    compressed_chunk_info_offset: usize,
    compressed_chunk_info_offset_field: usize,
}

#[derive(Clone)]
struct NameEntry {
    offset: usize,
    len: usize,
    name: String,
}

struct SummaryCursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> SummaryCursor<'a> {
    fn new(data: &'a [u8], position: usize) -> Self {
        Self { data, position }
    }

    fn i32(&mut self) -> Result<i32, String> {
        let value = read_i32(self.data, self.position)?;
        self.position += 4;
        Ok(value)
    }

    fn skip(&mut self, bytes: usize) -> Result<(), String> {
        self.position = self
            .position
            .checked_add(bytes)
            .ok_or("UPK summary offset overflow")?;
        if self.position > self.data.len() {
            return Err("UPK summary is truncated".into());
        }
        Ok(())
    }

    fn fstring(&mut self) -> Result<(), String> {
        let length = self.i32()?;
        let bytes = if length < 0 {
            usize::try_from(-i64::from(length))
                .map_err(|_| "Invalid wide UPK string length")?
                .checked_mul(2)
                .ok_or("UPK string length overflow")?
        } else {
            usize::try_from(length).map_err(|_| "Invalid UPK string length")?
        };
        self.skip(bytes)
    }

    fn array(&mut self, element_size: usize) -> Result<(), String> {
        let count = usize::try_from(self.i32()?).map_err(|_| "Negative UPK array count")?;
        self.skip(
            count
                .checked_mul(element_size)
                .ok_or("UPK array length overflow")?,
        )
    }
}

fn read_i32(data: &[u8], offset: usize) -> Result<i32, String> {
    let bytes = data
        .get(offset..offset + 4)
        .ok_or("UPK header is truncated")?;
    Ok(i32::from_le_bytes(bytes.try_into().unwrap()))
}

fn write_i32(data: &mut [u8], offset: usize, value: i32) -> Result<(), String> {
    let output = data
        .get_mut(offset..offset + 4)
        .ok_or("UPK write is outside the file")?;
    output.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_i64(data: &mut [u8], offset: usize, value: i64) -> Result<(), String> {
    let output = data
        .get_mut(offset..offset + 8)
        .ok_or("UPK write is outside the decrypted header")?;
    output.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_i64(data: &[u8], offset: usize) -> Result<i64, String> {
    let bytes = data
        .get(offset..offset + 8)
        .ok_or("UPK read is outside the decrypted header")?;
    Ok(i64::from_le_bytes(bytes.try_into().unwrap()))
}

fn chunk_table_stride(
    data: &[u8],
    table: usize,
    count: usize,
    licensee_version: u16,
) -> Result<usize, String> {
    let normal = 24usize;
    let padded = 36usize;
    if licensee_version >= 33 {
        return Ok(padded);
    }
    if count >= 2 {
        let first = read_i64(data, table + 4)?;
        let first_size = i64::from(read_i32(data, table + 12)?);
        let expected_second = first
            .checked_add(first_size)
            .ok_or("Compressed chunk offset overflow")?;
        if read_i64(data, table + 4 + padded).ok() == Some(expected_second) {
            return Ok(padded);
        }
    }
    Ok(normal)
}

fn read_info(data: &[u8]) -> Result<UpkInfo, String> {
    if data.len() < 57 || u32::from_le_bytes(data[0..4].try_into().unwrap()) != UPK_MAGIC {
        return Err("Invalid UPK magic number".into());
    }
    let total_header_size =
        usize::try_from(read_i32(data, 8)?).map_err(|_| "Negative total header size")?;
    let licensee_version = u16::from_le_bytes(data[6..8].try_into().unwrap());
    let name_count = usize::try_from(read_i32(data, 25)?).map_err(|_| "Negative name count")?;
    let name_offset = usize::try_from(read_i32(data, 29)?).map_err(|_| "Negative name offset")?;
    let export_count = usize::try_from(read_i32(data, 33)?).map_err(|_| "Negative export count")?;
    let export_offset =
        usize::try_from(read_i32(data, 37)?).map_err(|_| "Negative export offset")?;
    let import_count = usize::try_from(read_i32(data, 41)?).map_err(|_| "Negative import count")?;
    let import_offset =
        usize::try_from(read_i32(data, 45)?).map_err(|_| "Negative import offset")?;
    if name_offset < 12 || name_offset > data.len() {
        return Err(format!("UPK name offset {name_offset} is outside the file"));
    }
    // Parse the variable-length package summary. Reading these values at
    // name_offset-12 only works for one summary shape; on other valid packages
    // it reads padding zeros and later rewrites name-table bytes as chunk rows.
    let mut summary = SummaryCursor::new(data, 12);
    summary.fstring()?;
    let package_flags = summary.i32()? as u32;
    for _ in 0..7 {
        summary.i32()?;
    }
    for _ in 0..4 {
        summary.i32()?;
    }
    let package_guid_offset = summary.position;
    summary.skip(16)?;
    let generations = usize::try_from(read_i32(data, summary.position)?)
        .map_err(|_| "Negative UPK generation count")?;
    let generation_name_count_offset = if generations == 0 {
        None
    } else {
        Some(
            summary
                .position
                .checked_add(8)
                .and_then(|offset| {
                    (generations - 1)
                        .checked_mul(12)
                        .and_then(|size| offset.checked_add(size))
                })
                .ok_or("UPK generation offset overflow")?,
        )
    };
    summary.array(12)?;
    for _ in 0..3 {
        summary.i32()?;
    }
    summary.array(16)?;
    summary.i32()?;
    let string_count =
        usize::try_from(summary.i32()?).map_err(|_| "Negative summary string count")?;
    if string_count > 100_000 {
        return Err("UPK summary has too many strings".into());
    }
    for _ in 0..string_count {
        summary.fstring()?;
    }
    let entry_count =
        usize::try_from(summary.i32()?).map_err(|_| "Negative summary entry count")?;
    if entry_count > 100_000 {
        return Err("UPK summary has too many entries".into());
    }
    for _ in 0..entry_count {
        summary.skip(20)?;
        summary.array(4)?;
    }
    let generation_size_offset = summary.position;
    let generation_size = summary.i32()?;
    let compressed_chunk_info_offset_field = summary.position;
    let compressed_chunk_info_offset =
        usize::try_from(summary.i32()?).map_err(|_| "Negative compressed chunk table offset")?;
    let last_block_size =
        usize::try_from(summary.i32()?).map_err(|_| "Negative UPK last block size")?;
    let mut nonce = [0u8; 12];
    if licensee_version >= 33 {
        nonce.copy_from_slice(
            data.get(summary.position..summary.position + 12)
                .ok_or("UPK header nonce is truncated")?,
        );
        summary.skip(12)?;
    }
    if summary.position != name_offset && summary.position.checked_add(12) != Some(name_offset) {
        return Err(format!(
            "UPK summary fields end at {}, but name table starts at {name_offset}",
            summary.position
        ));
    }
    Ok(UpkInfo {
        licensee_version,
        package_flags,
        nonce,
        total_header_size,
        name_count,
        name_offset,
        export_count,
        export_offset,
        import_count,
        import_offset,
        package_guid_offset,
        generation_name_count_offset,
        generation_size,
        last_block_size,
        generation_size_offset,
        compressed_chunk_info_offset,
        compressed_chunk_info_offset_field,
    })
}

fn exported_names(data: &[u8], info: UpkInfo) -> Result<Vec<(usize, usize, bool)>, String> {
    let name_table = scan_names(data, info.name_count)?;
    let mut position = info
        .export_offset
        .checked_sub(info.name_offset)
        .ok_or("Export table precedes encrypted header")?;
    let mut names = Vec::with_capacity(info.export_count);
    for index in 0..info.export_count {
        let object_name = usize::try_from(read_i32(data, position + 12)?)
            .map_err(|_| format!("Export {index} has a negative object-name index"))?;
        if object_name >= info.name_count {
            return Err(format!("Export {index} has an invalid object-name index"));
        }
        let class_index = read_i32(data, position)?;
        let is_package = if class_index < 0 {
            let import = usize::try_from(-i64::from(class_index) - 1)
                .map_err(|_| "Invalid export class import")?;
            if import >= info.import_count {
                return Err(format!("Export {index} has an invalid class import"));
            }
            let import_start = info
                .import_offset
                .checked_sub(info.name_offset)
                .ok_or("Import table precedes encrypted header")?;
            let class_name = usize::try_from(read_i32(data, import_start + import * 28 + 20)?)
                .map_err(|_| "Invalid export class name")?;
            name_table
                .get(class_name)
                .ok_or("Invalid export class name")?
                .name
                .trim_end_matches('\0')
                .eq_ignore_ascii_case("Package")
        } else {
            false
        };
        names.push((position + 12, object_name, is_package));

        // FObjectExport is variable length because it ends with an array of
        // net-object indices. This mirrors the UE3/C# package reader.
        position = position
            .checked_add(if info.licensee_version >= 22 { 48 } else { 44 })
            .ok_or("Export table offset overflow")?;
        let net_object_count = usize::try_from(read_i32(data, position)?)
            .map_err(|_| format!("Export {index} has a negative net-object count"))?;
        position = position
            .checked_add(4 + net_object_count.saturating_mul(4) + 16 + 4)
            .ok_or("Export table offset overflow")?;
        if position > data.len() {
            return Err("Export table is truncated".into());
        }
    }
    Ok(names)
}

fn encrypted_size(info: UpkInfo) -> Result<(usize, usize), String> {
    let plain = usize::try_from(
        info.total_header_size as i64
            - if info.package_flags & 0x0800 != 0 {
                info.last_block_size as i64
            } else {
                info.generation_size as i64
            }
            - info.name_offset as i64,
    )
    .map_err(|_| "Invalid encrypted UPK header size")?;
    Ok((
        plain,
        if info.package_flags & 0x0800 != 0 {
            plain
        } else {
            plain.div_ceil(16) * 16
        },
    ))
}

fn object_path(
    data: &[u8],
    info: UpkInfo,
    exports: &[(usize, usize, bool)],
    index: i32,
) -> Result<String, String> {
    let names = scan_names(data, info.name_count)?;
    let mut current = index;
    let mut parts = Vec::new();
    let mut visited = std::collections::HashSet::new();
    while current != 0 {
        if !visited.insert(current) || visited.len() > 64 {
            return Err("Cyclic or excessive UPK object hierarchy".into());
        }
        let name_field = if current > 0 {
            exports
                .get(current as usize - 1)
                .ok_or("Invalid outer export")?
                .0
        } else {
            let index =
                usize::try_from(-i64::from(current) - 1).map_err(|_| "Invalid outer import")?;
            if index >= info.import_count {
                return Err("Invalid outer import".into());
            }
            info.import_offset - info.name_offset + index * 28 + 20
        };
        let name_index =
            usize::try_from(read_i32(data, name_field)?).map_err(|_| "Negative object name")?;
        let mut name = names
            .get(name_index)
            .ok_or("Invalid object name")?
            .name
            .clone();
        let instance = read_i32(data, name_field + 4)?;
        if instance > 0 {
            name.push_str(&format!("_{}", instance - 1));
        }
        parts.push(name);
        current = read_i32(data, name_field - 4)?;
    }
    parts.reverse();
    Ok(parts.join("."))
}

fn declared_asset_exports(data: &[u8], info: UpkInfo) -> Result<Vec<(usize, String)>, String> {
    let names = scan_names(data, info.name_count)?;
    let exports = exported_names(data, info)?;
    let mut assets = Vec::new();
    for (i, (field, _, _)) in exports.iter().enumerate() {
        let class = read_i32(data, field - 12)?;
        let class_name = if class < 0 {
            let index =
                usize::try_from(-i64::from(class) - 1).map_err(|_| "Invalid asset class")?;
            if index >= info.import_count {
                return Err("Invalid asset class import".into());
            }
            let at = info.import_offset - info.name_offset + index * 28 + 20;
            usize::try_from(read_i32(data, at)?).map_err(|_| "Invalid asset class name")?
        } else {
            continue;
        };
        let name = names
            .get(class_name)
            .ok_or("Invalid asset class name")?
            .name
            .as_str();
        if name.starts_with("ProductAsset_") || name.starts_with("ProductThumbnailAsset") {
            assets.push((i, object_path(data, info, &exports, i as i32 + 1)?));
        }
    }
    Ok(assets)
}

fn declared_asset_paths(data: &[u8], info: UpkInfo) -> Result<Vec<String>, String> {
    Ok(declared_asset_exports(data, info)?
        .into_iter()
        .map(|(_, path)| path)
        .collect())
}

fn declared_asset_path(data: &[u8], info: UpkInfo) -> Result<Option<String>, String> {
    let mut assets = declared_asset_paths(data, info)?;
    Ok((assets.len() == 1).then(|| assets.remove(0)))
}

pub(crate) fn thumbnail_asset_path(
    path: &Path,
    base_dir: &Path,
    main_asset: Option<&str>,
) -> Result<String, String> {
    let raw = fs::read(path).map_err(|e| e.to_string())?;
    let info = read_info(&raw)?;
    let (header, _, _) = find_key_and_decrypt(&raw, info, &keys(base_dir))?;
    let assets = declared_asset_paths(&header, info)?;
    if assets.len() == 1 {
        return Ok(assets[0].clone());
    }
    if let Some(leaf) = main_asset.and_then(|asset| asset.rsplit('.').next()) {
        let matching: Vec<_> = assets
            .iter()
            .filter(|asset| {
                asset.rsplit('.').next().is_some_and(|name| {
                    name.eq_ignore_ascii_case(&format!("{leaf}_T"))
                        || name.eq_ignore_ascii_case(&format!("{leaf}_TThumbnail"))
                })
            })
            .collect();
        if matching.len() == 1 {
            return Ok(matching[0].clone());
        }
    }
    Err(format!(
        "Could not uniquely resolve the item's thumbnail in {}",
        path.display()
    ))
}

fn align_asset_outer(
    data: &mut [u8],
    info: UpkInfo,
    target_path: &str,
    donor_index: Option<usize>,
) -> Result<(), String> {
    let (parent, leaf) = target_path
        .rsplit_once('.')
        .ok_or("Invalid target asset path")?;
    let names = scan_names(data, info.name_count)?;
    let exports = exported_names(data, info)?;
    let matching: Vec<_> = exports
        .iter()
        .enumerate()
        .filter(|(i, (_, index, package))| {
            !package
                && names[*index].name.eq_ignore_ascii_case(leaf)
                && donor_index.is_none_or(|donor| donor == *i)
        })
        .collect();
    if matching.len() != 1 {
        return Err(format!(
            "Target asset '{target_path}' has {} matching exports (donor export {donor_index:?})",
            matching.len()
        ));
    }
    let (asset, (name_field, _, _)) = matching[0];
    if object_path(data, info, &exports, asset as i32 + 1)?.eq_ignore_ascii_case(target_path) {
        return Ok(());
    }
    let mut outer = None;
    for (i, (_, _, is_package)) in exports.iter().enumerate() {
        if *is_package
            && object_path(data, info, &exports, i as i32 + 1)?.eq_ignore_ascii_case(parent)
        {
            if outer.is_some() {
                return Err("Ambiguous target package group".into());
            }
            outer = Some(i as i32 + 1);
        }
    }
    let outer = outer.ok_or_else(|| {
        format!("Donor has no package group '{parent}'; this layout cannot be safely swapped")
    })?;
    write_i32(data, name_field - 4, outer)
}

fn crypt(data: &[u8], key: &[u8; 32], encrypt: bool) -> Result<Vec<u8>, String> {
    if !data.len().is_multiple_of(16) {
        return Err("AES region is not block aligned".into());
    }
    let cipher = Aes256::new(GenericArray::from_slice(key));
    let mut output = data.to_vec();
    for chunk in output.chunks_exact_mut(16) {
        if encrypt {
            cipher.encrypt_block(GenericArray::from_mut_slice(chunk));
        } else {
            cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
        }
    }
    Ok(output)
}

fn crypt_for_info(
    data: &[u8],
    key: &[u8; 32],
    encrypt: bool,
    info: UpkInfo,
) -> Result<Vec<u8>, String> {
    if info.package_flags & 0x0800 != 0 {
        crate::patcher::upk_package::crypt_ctr(data, key, &info.nonce)
    } else {
        crypt(data, key, encrypt)
    }
}

fn keys(base_dir: &Path) -> Vec<[u8; 32]> {
    let mut keys = vec![DEFAULT_KEY];
    let _ = base_dir;
    if let Ok(embedded) = crate::upk_keys::embedded() {
        for (_, key) in embedded {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

fn scan_names(data: &[u8], count: usize) -> Result<Vec<NameEntry>, String> {
    let mut entries = Vec::with_capacity(count);
    let mut offset = 0usize;
    for index in 0..count {
        let len = usize::try_from(read_i32(data, offset)?)
            .map_err(|_| format!("Name table entry {index} has a negative length"))?;
        if len == 0 || len > 512 || offset + 4 + len + 8 > data.len() {
            return Err(format!(
                "Name table entry {index} has invalid length {len} at {offset}"
            ));
        }
        let raw_name = &data[offset + 4..offset + 4 + len.saturating_sub(1)];
        let name: String = raw_name.iter().map(|byte| char::from(*byte)).collect();
        entries.push(NameEntry { offset, len, name });
        offset += 4 + len + 8;
    }
    Ok(entries)
}

fn find_key_and_decrypt(
    raw: &[u8],
    info: UpkInfo,
    candidates: &[[u8; 32]],
) -> Result<(Vec<u8>, [u8; 32], usize), String> {
    let (_, aligned) = encrypted_size(info)?;
    let encrypted = raw
        .get(info.name_offset..info.name_offset + aligned)
        .ok_or("UPK is too small for its encrypted header")?;
    for key in candidates {
        let decrypted = crypt_for_info(encrypted, key, false, info)?;
        if scan_names(&decrypted, info.name_count).is_ok() {
            return Ok((decrypted, *key, aligned));
        }
    }
    Err("No embedded key could decrypt this cosmetic UPK".into())
}

fn replace_name(
    data: Vec<u8>,
    index: usize,
    new: &str,
    count: usize,
) -> Result<(Vec<u8>, i32), String> {
    let entry = scan_names(&data, count)?
        .get(index)
        .cloned()
        .ok_or("Invalid replacement name index")?;
    let mut replacement = new.as_bytes().to_vec();
    replacement.push(0);
    if replacement.len() == entry.len {
        let mut output = data;
        output[entry.offset + 4..entry.offset + 4 + replacement.len()]
            .copy_from_slice(&replacement);
        return Ok((output, 0));
    }
    let delta = i32::try_from(replacement.len()).map_err(|_| "UPK name is too long")?
        - i32::try_from(entry.len).map_err(|_| "UPK name is too long")?;
    let mut output = Vec::new();
    output.extend_from_slice(&data[..entry.offset]);
    output.extend_from_slice(&(replacement.len() as i32).to_le_bytes());
    output.extend_from_slice(&replacement);
    output.extend_from_slice(&data[entry.offset + 4 + entry.len..]);
    Ok((output, delta))
}

/// Plan against the original table so renames cannot consume one another.
/// A package and its asset can share an FName slot in the donor but require
/// distinct names in the target. Split that slot and redirect the asset exports,
/// leaving package references and all existing name indexes intact.
fn rename_identities(
    mut data: Vec<u8>,
    info: UpkInfo,
    pairs: &[(String, String)],
    donor_export_name: Option<&str>,
    target_export_name: &str,
) -> Result<(Vec<u8>, i32, usize), String> {
    let original = scan_names(&data, info.name_count)?;
    let exports = exported_names(&data, info)?;
    let replacements: Vec<Option<&str>> = original
        .iter()
        .map(|entry| {
            pairs.iter().find_map(|(old, new)| {
                entry
                    .name
                    .trim_end_matches('\0')
                    .eq_ignore_ascii_case(old)
                    .then_some(new.as_str())
            })
        })
        .collect();
    let mut alias = None;
    if let Some(donor_name) = donor_export_name {
        let matching: Vec<_> = exports
            .iter()
            .filter(|(_, index, is_package)| {
                !is_package
                    && original[*index]
                        .name
                        .trim_end_matches('\0')
                        .eq_ignore_ascii_case(donor_name)
            })
            .collect();
        if matching.is_empty() {
            return Err(format!(
                "Donor UPK does not export source object '{donor_name}'"
            ));
        }
        for (offset, index, _) in matching {
            let renamed =
                replacements[*index].unwrap_or(original[*index].name.trim_end_matches('\0'));
            if renamed != target_export_name {
                write_i32(
                    &mut data,
                    *offset,
                    i32::try_from(info.name_count).map_err(|_| "Too many UPK names")?,
                )?;
                let entry = &original[*index];
                alias = Some(
                    data[entry.offset + 4 + entry.len..entry.offset + 4 + entry.len + 8].to_vec(),
                );
            }
        }
    }
    let mut total_delta = 0i32;
    for (index, replacement) in replacements.iter().enumerate() {
        if let Some(new) = replacement {
            let (renamed, delta) = replace_name(data, index, new, info.name_count)?;
            data = renamed;
            total_delta = total_delta
                .checked_add(delta)
                .ok_or("UPK name table growth overflow")?;
        }
    }
    let mut count = info.name_count;
    if let Some(flags) = alias {
        let names = scan_names(&data, count)?;
        let last = names.last().ok_or("UPK has no names")?;
        let end = last.offset + 4 + last.len + 8;
        let mut entry = Vec::new();
        entry.extend_from_slice(
            &i32::try_from(target_export_name.len() + 1)
                .map_err(|_| "UPK object name is too long")?
                .to_le_bytes(),
        );
        entry.extend_from_slice(target_export_name.as_bytes());
        entry.push(0);
        entry.extend_from_slice(&flags);
        total_delta = total_delta
            .checked_add(i32::try_from(entry.len()).map_err(|_| "UPK name entry is too large")?)
            .ok_or("UPK name table growth overflow")?;
        data.splice(end..end, entry);
        count += 1;
    }
    Ok((data, total_delta, count))
}

fn apply_deltas(
    raw: &mut [u8],
    decrypted: &mut [u8],
    info: UpkInfo,
    delta: i32,
) -> Result<(), String> {
    if delta == 0 {
        return Ok(());
    }
    let (old_plain, old_aligned) = encrypted_size(info)?;
    let new_plain = usize::try_from(old_plain as i64 + delta as i64)
        .map_err(|_| "Renamed header became negative")?;
    let new_aligned = if info.package_flags & 0x0800 != 0 {
        new_plain
    } else {
        new_plain.div_ceil(16) * 16
    };
    let alignment_delta = new_aligned as i64 - old_aligned as i64;
    write_i32(
        raw,
        info.compressed_chunk_info_offset_field,
        i32::try_from(info.compressed_chunk_info_offset as i64 + delta as i64)
            .map_err(|_| "Chunk table offset overflow")?,
    )?;
    for offset in [37usize, 45, 49, 53] {
        let value = read_i32(raw, offset)?;
        if value > info.name_offset as i32 {
            write_i32(raw, offset, value + delta)?;
        }
    }
    write_i32(
        raw,
        info.generation_size_offset,
        i32::try_from(info.generation_size as i64 + alignment_delta - delta as i64)
            .map_err(|_| "Generation size overflow")?,
    )?;
    // The compression reader's plaintext boundary moves with the name table.
    // Leaving virtual chunk/export addresses unchanged makes the engine decrypt
    // the first `delta` bytes of object data again (or read header bytes as data).
    let updated = UpkInfo {
        name_count: usize::try_from(read_i32(raw, 25)?).map_err(|_| "Negative name count")?,
        export_offset: usize::try_from(info.export_offset as i64 + i64::from(delta))
            .map_err(|_| "Export table offset became negative")?,
        import_offset: usize::try_from(info.import_offset as i64 + i64::from(delta))
            .map_err(|_| "Import table offset became negative")?,
        ..info
    };
    for (name_field, _, _) in exported_names(decrypted, updated)? {
        let serial_field = name_field + 24;
        let serial_size = read_i32(decrypted, name_field + 20)?;
        if serial_size > 0 {
            let old = read_i64(decrypted, serial_field)?;
            let new = old
                .checked_add(i64::from(delta))
                .ok_or("Export offset overflow")?;
            if new < 0 {
                return Err("Export offset became negative".into());
            }
            write_i64(decrypted, serial_field, new)?;
        }
    }
    {
        let table = usize::try_from(info.compressed_chunk_info_offset as i64 + delta as i64)
            .map_err(|_| "Chunk table offset became negative")?;
        let count = usize::try_from(read_i32(decrypted, table)?)
            .map_err(|_| "Negative compressed chunk count")?;
        let stride = chunk_table_stride(decrypted, table, count, info.licensee_version)?;
        let mut row = table + 4;
        for _ in 0..count {
            let logical = read_i64(decrypted, row)?;
            write_i64(
                decrypted,
                row,
                logical
                    .checked_add(i64::from(delta))
                    .ok_or("Logical chunk offset overflow")?,
            )?;
            let old = i64::from_le_bytes(
                decrypted
                    .get(row + 12..row + 20)
                    .ok_or("Compressed chunk table is truncated")?
                    .try_into()
                    .unwrap(),
            );
            write_i64(decrypted, row + 12, old + alignment_delta)?;
            row += stride;
        }
        write_i32(
            raw,
            8,
            i32::try_from(info.total_header_size as i64 + alignment_delta)
                .map_err(|_| "Total header size overflow")?,
        )?;
    }
    Ok(())
}

// Texture mip offsets can refer to a shared .tfc or to this package. Only local
// offsets move with the virtual data region; pixels and external offsets stay put.
fn rebase_texture_bulk(source: &Path, delta: i32) -> Result<Option<Vec<u8>>, String> {
    use crate::patcher::upk_package::{UpkPackage, strip};
    let mut package = UpkPackage::load(source)?;
    let mut patches = Vec::new();
    for export in &package.exports {
        if strip(&package.class_of(export)) != "Texture2D" {
            continue;
        }
        let (_, native) = package.serialized_props(export)?;
        let serial =
            &package.image[export.serial_offset..export.serial_offset + export.serial_size];
        let mut at = native;
        let mut record = |at: &mut usize| -> Result<(), String> {
            let flags = read_i32(serial, *at)? as u32;
            let count = read_i32(serial, *at + 4)?;
            let stored = read_i32(serial, *at + 8)?;
            *at += 12;
            let size = if flags & 32 != 0 && count == 0 && stored == -1 {
                0
            } else {
                usize::try_from(stored).map_err(|_| "Negative texture bulk size")?
            };
            if flags & 0x10000 == 0 || flags & 1 != 0 {
                let old = read_i64(serial, *at)?;
                if flags & (1 | 32) == 0 && size > 0 {
                    // Current cooked inline records must point to their own bytes.
                    if old != (export.serial_offset + *at + 8) as i64 {
                        return Err("Unsupported out-of-line local texture bulk data".into());
                    }
                    patches.push((
                        export.serial_offset + *at,
                        old.checked_add(i64::from(delta))
                            .ok_or("Texture bulk offset overflow")?,
                    ));
                }
                *at += 8;
            }
            if flags & (1 | 32) == 0 {
                *at = at.checked_add(size).ok_or("Texture bulk size overflow")?;
            }
            if *at > serial.len() {
                return Err("Truncated texture bulk data".into());
            }
            Ok(())
        };
        record(&mut at)?;
        let mips = read_i32(serial, at)?;
        at += 4;
        if !(0..=32).contains(&mips) {
            return Err("Unsupported texture mip layout".into());
        }
        for _ in 0..mips {
            record(&mut at)?;
            at = at.checked_add(8).ok_or("Texture dimensions overflow")?;
            if at > serial.len() {
                return Err("Truncated texture mip dimensions".into());
            }
        }
    }
    if patches.is_empty() {
        return Ok(None);
    }
    for (offset, value) in patches {
        package.patch(offset, &value.to_le_bytes())?;
    }
    package.repacked().map(Some)
}

fn infer_explosion_pairs(
    donor_upk: &str,
    donor_asset: &str,
    target_upk: &str,
    target_asset: &str,
) -> Vec<(String, String)> {
    let donor: Vec<&str> = donor_asset.split('.').collect();
    let target: Vec<&str> = target_asset.split('.').collect();
    let target_lower: Vec<String> = target
        .iter()
        .map(|part| part.to_ascii_lowercase())
        .collect();
    let mut pairs = Vec::new();
    let mut seen = Vec::<String>::new();
    for index in 0..donor.len() {
        let old = donor[index];
        let new = if index + 1 == donor.len() {
            target.last().copied()
        } else {
            target.get(index).copied()
        };
        let Some(new) = new else { continue };
        let important = index == 0
            || index + 1 == donor.len()
            || target_lower.contains(&old.to_ascii_lowercase());
        if important && !old.is_empty() && !new.is_empty() {
            seen.push(old.to_string());
            pairs.push((old.to_string(), new.to_string()));
        }
    }
    let donor_stem = Path::new(donor_upk)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(donor_upk);
    let target_stem = Path::new(target_upk)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(target_upk);
    if donor_stem != target_stem && !seen.iter().any(|value| value == donor_stem) {
        pairs.push((donor_stem.to_string(), target_stem.to_string()));
    }
    pairs
}

fn logical_upk_stem(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    let without_backup = name.strip_suffix(".bak").unwrap_or(name);
    without_backup.strip_suffix(".upk").or_else(|| {
        Path::new(without_backup)
            .file_stem()
            .and_then(|value| value.to_str())
    })
}

fn validate_output(
    output: &[u8],
    target_key: &[u8; 32],
    target_name: &str,
    target_short: &str,
    target_export_name: &str,
    target_guid: &[u8; 16],
    alternative_targets: &[String],
    target_asset_path: Option<&str>,
) -> Result<(), String> {
    let info = read_info(output)?;
    if output.get(info.package_guid_offset..info.package_guid_offset + 16)
        != Some(target_guid.as_slice())
    {
        return Err("Generated UPK does not preserve the target package GUID".into());
    }
    let (_, aligned) = encrypted_size(info)?;
    let encrypted = output
        .get(info.name_offset..info.name_offset + aligned)
        .ok_or("Generated UPK encrypted header is truncated")?;
    let decrypted = crypt_for_info(encrypted, target_key, false, info)?;
    let names = scan_names(&decrypted, info.name_count)?;
    let export_names = exported_names(&decrypted, info)?;
    let has_target_name = names
        .iter()
        .any(|entry| entry.name.trim_end_matches('\0') == target_name);
    let has_target_short = target_short == target_name
        || names
            .iter()
            .any(|entry| entry.name.trim_end_matches('\0') == target_short);
    let has_alternative = alternative_targets.iter().any(|target| {
        names
            .iter()
            .any(|entry| entry.name.trim_end_matches('\0') == target)
    });
    if !has_target_name && !has_alternative {
        return Err(format!(
            "Generated UPK does not contain target package identity '{}'{}",
            target_name,
            if alternative_targets.is_empty() {
                String::new()
            } else {
                format!(" or one of: {}", alternative_targets.join(", "))
            }
        ));
    }
    if has_target_name && !has_target_short && !has_alternative {
        return Err(format!(
            "Generated UPK does not contain target package name '{target_short}'"
        ));
    }
    if !export_names.iter().any(|(_, index, is_package)| {
        !is_package
            && names
                .get(*index)
                .is_some_and(|entry| entry.name.trim_end_matches('\0') == target_export_name)
    }) {
        return Err(format!(
            "Generated UPK does not export target object '{target_export_name}'"
        ));
    }
    if let Some(path) = target_asset_path {
        let mut found = false;
        for (i, (_, _, package)) in export_names.iter().enumerate() {
            if !package
                && object_path(&decrypted, info, &export_names, i as i32 + 1)?
                    .eq_ignore_ascii_case(path)
            {
                found = true;
                break;
            }
        }
        if !found {
            return Err(format!(
                "Generated UPK does not export full target path '{path}'"
            ));
        }
    }

    let chunk_count = usize::try_from(read_i32(&decrypted, info.compressed_chunk_info_offset)?)
        .map_err(|_| "Generated UPK has a negative compressed-chunk count")?;
    if chunk_count == 0 || chunk_count >= 1_000 {
        return Err("Generated UPK has an invalid compressed-chunk count".into());
    }
    let mut row = info.compressed_chunk_info_offset + 4;
    let logical_start = read_i64(&decrypted, row)?;
    if logical_start != (info.name_offset + info.compressed_chunk_info_offset) as i64
        || logical_start != i64::from(read_i32(output, 49)?)
    {
        return Err("Generated UPK header overlaps or leaves a gap before object data".into());
    }
    for (name_field, _, _) in &export_names {
        if read_i32(&decrypted, name_field + 20)? > 0
            && read_i64(&decrypted, name_field + 24)? < logical_start
        {
            return Err("Generated UPK export points into its header".into());
        }
    }
    let stride = chunk_table_stride(
        &decrypted,
        info.compressed_chunk_info_offset,
        chunk_count,
        info.licensee_version,
    )?;
    let mut valid_chunks = 0usize;
    for index in 0..chunk_count {
        let compressed_offset = usize::try_from(i64::from_le_bytes(
            decrypted
                .get(row + 12..row + 20)
                .ok_or("Generated UPK compressed-chunk table is truncated")?
                .try_into()
                .unwrap(),
        ))
        .map_err(|_| format!("Generated UPK chunk {index} has a negative offset"))?;
        let compressed_size = usize::try_from(read_i32(&decrypted, row + 20)?)
            .map_err(|_| format!("Generated UPK chunk {index} has a negative size"))?;
        if info.package_flags & 0x0800 != 0 {
            let nonce: [u8; 12] = decrypted
                .get(row + 24..row + 36)
                .ok_or("Generated UPK chunk nonce is truncated")?
                .try_into()
                .unwrap();
            let compressed = output
                .get(compressed_offset..compressed_offset + compressed_size)
                .ok_or_else(|| format!("Generated UPK chunk {index} extends past the file"))?;
            let decoded = crate::patcher::upk_package::crypt_ctr(compressed, target_key, &nonce)?;
            crate::patch_core::upk::decomp_chunk_at(&decoded, 0)
                .map_err(|error| format!("Generated UPK chunk {index} is invalid: {error:?}"))?;
        } else {
            let (_, _, end) = crate::patch_core::upk::decomp_chunk_at(output, compressed_offset)
                .map_err(|error| {
                    format!(
                        "Generated UPK chunk {index} at {compressed_offset} is invalid: {error:?}"
                    )
                })?;
            if end > output.len() {
                return Err(format!("Generated UPK chunk {index} extends past the file"));
            }
        }
        valid_chunks += 1;
        row += stride;
    }

    if valid_chunks == 0 {
        return Err("Generated UPK contains no readable compressed chunks".into());
    }

    Ok(())
}

/// Append property names while preserving every existing FName index. Uses the
/// same header/chunk and local texture-offset rebasing as cosmetic identities.
pub(crate) fn append_names(
    source: &Path,
    destination: &Path,
    additions: &[&str],
) -> Result<(), String> {
    use crate::patcher::upk_package::{UpkPackage, strip};
    let original = UpkPackage::load(source)?;
    let mut raw = fs::read(source).map_err(|e| e.to_string())?;
    let info = read_info(&raw)?;
    if info.licensee_version < 22 {
        return Err("Paint properties require the current Rocket League package layout".into());
    }
    let candidates = keys(source.parent().unwrap_or(Path::new(".")));
    let (mut decrypted, key, old_encrypted_size) = find_key_and_decrypt(&raw, info, &candidates)?;
    let entries = scan_names(&decrypted, info.name_count)?;
    let last = entries.last().ok_or("Empty UPK name table")?;
    let at = last.offset + 4 + last.len + 8;
    let mut added = Vec::new();
    let mut wanted = std::collections::HashSet::new();
    for name in additions {
        if !wanted.insert(*name) || entries.iter().any(|e| e.name == *name) {
            continue;
        }
        if name.is_empty() || !name.is_ascii() || name.len() > 500 || name.contains('\0') {
            return Err("Invalid appended UPK name".into());
        }
        added.extend(((name.len() + 1) as i32).to_le_bytes());
        added.extend(name.as_bytes());
        added.push(0);
        added.extend([0u8; 8]);
    }
    let extra_count = additions
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>()
        .iter()
        .filter(|name| !entries.iter().any(|e| e.name == **name))
        .count();
    if extra_count == 0 {
        fs::copy(source, destination).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let delta = i32::try_from(added.len()).map_err(|_| "Appended names too large")?;
    if let Some(rebased) = rebase_texture_bulk(source, delta)? {
        raw = rebased;
        decrypted = find_key_and_decrypt(&raw, info, &candidates)?.0;
    }
    decrypted.splice(at..at, added);
    let count = i32::try_from(info.name_count + extra_count).map_err(|_| "Too many UPK names")?;
    write_i32(&mut raw, 25, count)?;
    if let Some(at) = info.generation_name_count_offset {
        write_i32(&mut raw, at, count)?;
    }
    apply_deltas(&mut raw, &mut decrypted, info, delta)?;
    let (plain, _) = encrypted_size(info)?;
    let new_plain = plain + delta as usize;
    let size = if info.package_flags & 0x0800 != 0 {
        new_plain
    } else {
        new_plain.div_ceil(16) * 16
    };
    decrypted.resize(size, 0);
    let encrypted = crypt_for_info(&decrypted[..size], &key, true, info)?;
    let mut output = raw[..info.name_offset].to_vec();
    output.extend(encrypted);
    output.extend_from_slice(&raw[info.name_offset + old_encrypted_size..]);
    fs::write(destination, output).map_err(|e| e.to_string())?;
    let check = UpkPackage::load(destination)?;
    if check.exports.len() != original.exports.len()
        || check.names[..original.names.len()] != original.names
    {
        return Err("Paint name-table verification failed".into());
    }
    for (before, after) in original.exports.iter().zip(&check.exports) {
        if before.serial_size != after.serial_size
            || (before.serial_size > 0
                && after.serial_offset != before.serial_offset + delta as usize)
        {
            return Err("Paint name-table export relocation failed".into());
        }
        if !matches!(
            strip(&original.class_of(before)),
            "Texture2D" | "TextureCube"
        ) && original.image[before.serial_offset..before.serial_offset + before.serial_size]
            != check.image[after.serial_offset..after.serial_offset + after.serial_size]
        {
            return Err("Paint name-table changed an unrelated export".into());
        }
    }
    Ok(())
}

/// A body swap carries the donor's boost layout along with its geometry. The
/// ProductAsset selects the emitter sockets; the skeletal mesh holds their
/// references and bone transforms; socket exports hold the position/rotation.
/// Keep these together, including single/dual emitters and ambient exhaust.
pub(crate) fn verify_body_boost_layout(
    donor: &crate::patcher::upk_package::UpkPackage,
    swapped: &crate::patcher::upk_package::UpkPackage,
) -> Result<(), String> {
    use crate::patcher::upk_package::strip;
    if !donor
        .exports
        .iter()
        .any(|e| strip(&donor.class_of(e)) == "ProductAsset_Body_TA")
    {
        return Ok(());
    }
    if donor.exports.len() != swapped.exports.len() {
        return Err("Body swap changed the donor's boost attachment exports".into());
    }
    for before in &donor.exports {
        let class = donor.class_of(before);
        if !matches!(
            strip(&class),
            "ProductAsset_Body_TA" | "SkeletalMesh" | "SkeletalMeshSocket"
        ) {
            continue;
        }
        let after = &swapped.exports[before.table_index];
        // The main ProductAsset can move to the target package group. Mesh and
        // socket ownership must still follow the donor's original references.
        if before.class_index != after.class_index
            || (strip(&class) != "ProductAsset_Body_TA" && before.outer_index != after.outer_index)
            || donor.image[before.serial_offset..before.serial_offset + before.serial_size]
                != swapped.image[after.serial_offset..after.serial_offset + after.serial_size]
        {
            return Err(format!(
                "Body swap did not preserve the donor's boost layout ({class}, export {})",
                before.table_index
            ));
        }
        for prop in donor.serialized_props(before)?.0 {
            let at = before.serial_offset + prop.value_offset;
            let name_offsets =
                if strip(&class) == "SkeletalMeshSocket" && prop.tag_type == "NameProperty" {
                    vec![at]
                } else if prop.name == "BoostEmitterSockets" && prop.tag_type == "ArrayProperty" {
                    let count = usize::try_from(donor.read_int(at)?)
                        .map_err(|_| "Negative boost emitter count")?;
                    if count.checked_mul(8).and_then(|n| n.checked_add(4)) != Some(prop.size) {
                        return Err("Invalid body boost emitter sockets".into());
                    }
                    (0..count).map(|i| at + 4 + i * 8).collect()
                } else {
                    Vec::new()
                };
            for offset in name_offsets {
                let index = usize::try_from(donor.read_int(offset)?)
                    .map_err(|_| "Invalid boost attachment name")?;
                if donor.names.get(index).is_none()
                    || donor.names.get(index) != swapped.names.get(index)
                {
                    return Err("Body swap changed a boost socket or bone name".into());
                }
            }
        }
    }
    Ok(())
}

pub fn patch_for_target(
    source: &Path,
    target_backup: &Path,
    destination: &Path,
    base_dir: &Path,
    donor_asset_path: Option<&str>,
    target_asset_path: Option<&str>,
) -> Result<(), String> {
    let mut raw = fs::read(source).map_err(|error| format!("{}: {error}", source.display()))?;
    let info = read_info(&raw)?;
    if info.licensee_version < 22 {
        return Err(
            "Cosmetic swaps require the current 64-bit Rocket League package layout".into(),
        );
    }
    let candidates = keys(base_dir);
    let (mut decrypted, source_key, old_encrypted_size) =
        find_key_and_decrypt(&raw, info, &candidates)?;
    let has_body = scan_names(&decrypted, info.name_count)?
        .iter()
        .any(|entry| entry.name == "ProductAsset_Body_TA");
    let target_raw =
        fs::read(target_backup).map_err(|error| format!("{}: {error}", target_backup.display()))?;
    let target_info = read_info(&target_raw)?;
    let (target_decrypted, target_key, _) =
        find_key_and_decrypt(&target_raw, target_info, &candidates)
            .map_err(|error| format!("Cannot verify pristine target UPK: {error}"))?;
    // Thumbnail filenames need not match their ProductThumbnailAsset export.
    // Missing catalog paths are resolved from the actual package declaration.
    let inferred_donor = if donor_asset_path.is_none() {
        declared_asset_path(&decrypted, info)?
    } else {
        None
    };
    let inferred_target = if target_asset_path.is_none() {
        declared_asset_path(&target_decrypted, target_info)?
    } else {
        None
    };
    let donor_asset_path = donor_asset_path.or(inferred_donor.as_deref());
    let target_asset_path = target_asset_path.or(inferred_target.as_deref());
    let target_guid: [u8; 16] = target_raw
        .get(target_info.package_guid_offset..target_info.package_guid_offset + 16)
        .ok_or("Target UPK package GUID is truncated")?
        .try_into()
        .unwrap();
    raw.get_mut(info.package_guid_offset..info.package_guid_offset + 16)
        .ok_or("Donor UPK package GUID is truncated")?
        .copy_from_slice(&target_guid);

    let source_name = logical_upk_stem(source).ok_or("Source UPK has no valid filename")?;
    let target_name = logical_upk_stem(destination).ok_or("Target UPK has no valid filename")?;
    let pairs =
        if let (Some(donor_asset), Some(target_asset)) = (donor_asset_path, target_asset_path) {
            infer_explosion_pairs(source_name, donor_asset, target_name, target_asset)
        } else {
            let source_short = source_name.strip_suffix("_SF").unwrap_or(source_name);
            let target_short = target_name.strip_suffix("_SF").unwrap_or(target_name);
            vec![
                (source_name.to_string(), target_name.to_string()),
                (source_short.to_string(), target_short.to_string()),
            ]
        };
    if !scan_names(&decrypted, info.name_count)?
        .iter()
        .any(|entry| {
            pairs
                .iter()
                .any(|(old, _)| entry.name.trim_end_matches('\0').eq_ignore_ascii_case(old))
        })
    {
        return Err(format!(
            "The donor package identity '{source_name}' was not found in its UPK name table"
        ));
    }
    let alternative_targets: Vec<String> =
        if donor_asset_path.is_some() && target_asset_path.is_some() {
            pairs.iter().map(|(_, new)| new.clone()).collect()
        } else {
            Vec::new()
        };
    let target_short = target_name.strip_suffix("_SF").unwrap_or(target_name);
    let target_export_name = target_asset_path
        .and_then(|asset| asset.split('.').next_back())
        .filter(|name| !name.is_empty())
        .unwrap_or(target_short);
    let donor_export_name = donor_asset_path
        .and_then(|asset| asset.split('.').next_back())
        .filter(|name| !name.is_empty());
    let donor_index = if let Some(path) = donor_asset_path {
        let exports = exported_names(&decrypted, info)?;
        let mut matches = Vec::new();
        for (i, (_, _, package)) in exports.iter().enumerate() {
            if !package
                && object_path(&decrypted, info, &exports, i as i32 + 1)?.eq_ignore_ascii_case(path)
            {
                matches.push(i);
            }
        }
        if matches.len() > 1 {
            // Cooked wheels can have a mesh and ProductAsset with the same
            // object path. The requested item identity belongs to ProductAsset.
            let products = declared_asset_exports(&decrypted, info)?;
            matches.retain(|i| products.iter().any(|(index, _)| index == i));
        }
        if matches.len() != 1 {
            return Err(format!("Donor UPK does not uniquely export '{path}'"));
        }
        Some(matches[0])
    } else {
        None
    };
    let (mut new_data, total_delta, name_count) = rename_identities(
        decrypted,
        info,
        &pairs,
        donor_export_name,
        target_export_name,
    )?;
    if total_delta != 0 {
        if let Some(rebased) = rebase_texture_bulk(source, total_delta)? {
            raw = rebased;
            raw[info.package_guid_offset..info.package_guid_offset + 16]
                .copy_from_slice(&target_guid);
            let (header, _, _) = find_key_and_decrypt(&raw, info, &candidates)?;
            new_data =
                rename_identities(header, info, &pairs, donor_export_name, target_export_name)?.0;
        }
    }
    decrypted = new_data;
    write_i32(
        &mut raw,
        25,
        i32::try_from(name_count).map_err(|_| "Too many UPK names")?,
    )?;
    if name_count != info.name_count {
        if let Some(offset) = info.generation_name_count_offset {
            write_i32(
                &mut raw,
                offset,
                i32::try_from(name_count).map_err(|_| "Too many UPK names")?,
            )?;
        }
    }
    apply_deltas(&mut raw, &mut decrypted, info, total_delta)?;
    if let Some(path) = target_asset_path {
        align_asset_outer(&mut decrypted, read_info(&raw)?, path, donor_index)?;
    }
    let (old_plain, _) = encrypted_size(info)?;
    let new_plain = usize::try_from(old_plain as i64 + total_delta as i64)
        .map_err(|_| "Renamed UPK header became negative")?;
    let new_encrypted_size = if info.package_flags & 0x0800 != 0 {
        new_plain
    } else {
        new_plain.div_ceil(16) * 16
    };
    decrypted.resize(new_encrypted_size, 0);
    let encrypted = crypt_for_info(&decrypted[..new_encrypted_size], &target_key, true, info)?;
    let mut output = Vec::with_capacity(raw.len() - old_encrypted_size + new_encrypted_size);
    output.extend_from_slice(&raw[..info.name_offset]);
    output.extend_from_slice(&encrypted);
    output.extend_from_slice(&raw[info.name_offset + old_encrypted_size..]);
    if info.package_flags & 0x0800 != 0 && source_key != target_key {
        let updated_info = read_info(&output)?;
        let header = crypt_for_info(
            output
                .get(updated_info.name_offset..updated_info.name_offset + new_encrypted_size)
                .ok_or("Generated encrypted header is truncated")?,
            &target_key,
            false,
            updated_info,
        )?;
        let count = usize::try_from(read_i32(
            &header,
            updated_info.compressed_chunk_info_offset,
        )?)
        .map_err(|_| "Negative compressed chunk count")?;
        let table = updated_info.compressed_chunk_info_offset + 4;
        let stride = chunk_table_stride(
            &header,
            updated_info.compressed_chunk_info_offset,
            count,
            updated_info.licensee_version,
        )?;
        for index in 0..count {
            let row = table + index * stride;
            let offset = usize::try_from(read_i64(&header, row + 12)?)
                .map_err(|_| "Negative compressed chunk offset")?;
            let size = usize::try_from(read_i32(&header, row + 20)?)
                .map_err(|_| "Negative compressed chunk size")?;
            let nonce: [u8; 12] = header
                .get(row + 24..row + 36)
                .ok_or("Compressed chunk nonce is truncated")?
                .try_into()
                .unwrap();
            let chunk = output
                .get_mut(offset..offset + size)
                .ok_or("Compressed chunk extends past the file")?;
            let plain = crate::patcher::upk_package::crypt_ctr(chunk, &source_key, &nonce)?;
            let encrypted = crate::patcher::upk_package::crypt_ctr(&plain, &target_key, &nonce)?;
            chunk.copy_from_slice(&encrypted);
        }
    }
    validate_output(
        &output,
        &target_key,
        target_name,
        target_short,
        target_export_name,
        &target_guid,
        &alternative_targets,
        target_asset_path,
    )?;

    let temp_name = format!(
        "{}.swapping.tmp",
        destination
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("cosmetic.upk")
    );
    let temp_path = destination.with_file_name(temp_name);
    fs::write(&temp_path, &output)
        .map_err(|error| format!("Failed to write {}: {error}", temp_path.display()))?;
    let written = fs::read(&temp_path)
        .map_err(|error| format!("Failed to verify {}: {error}", temp_path.display()))?;
    if let Err(error) = validate_output(
        &written,
        &target_key,
        target_name,
        target_short,
        target_export_name,
        &target_guid,
        &alternative_targets,
        target_asset_path,
    ) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    if has_body {
        let validation = (|| {
            let donor = crate::patcher::upk_package::UpkPackage::load(source)?;
            let swapped = crate::patcher::upk_package::UpkPackage::load(&temp_path)?;
            verify_body_boost_layout(&donor, &swapped)
        })();
        if let Err(error) = validation {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
    }
    let result = fs::copy(&temp_path, destination)
        .map_err(|error| format!("Failed to install {}: {error}", destination.display()));
    let _ = fs::remove_file(&temp_path);
    result.map(|_| ())
}

/// Extend the real import table, preserving all existing indices. New imported
/// material references let one ProductAsset change without replacing its peers.
pub fn extend_asset_imports(
    source: &Path,
    destination: &Path,
    base_dir: &Path,
    additional_names: &[String],
    objects: &[(String, String, String)],
) -> Result<(), String> {
    use std::collections::HashMap;
    let mut raw = fs::read(source).map_err(|e| e.to_string())?;
    let info = read_info(&raw)?;
    if info.licensee_version < 22 {
        return Err("Asset imports require current Rocket League packages".into());
    }
    let candidates = keys(base_dir);
    let (original, key, _) = find_key_and_decrypt(&raw, info, &candidates)?;
    let entries = scan_names(&original, info.name_count)?;
    let insert = entries
        .last()
        .map(|entry| entry.offset + 4 + entry.len + 8)
        .ok_or("Missing name table")?;
    let mut names: HashMap<String, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.name.clone(), i))
        .collect();
    let mut encoded_names = Vec::new();
    let mut next_name_index = info.name_count;
    let mut name_index = |name: &str| -> Result<usize, String> {
        if let Some(index) = names.get(name) {
            return Ok(*index);
        }
        if name.is_empty() || !name.is_ascii() || name.len() > 500 {
            return Err("Unsupported imported asset name".into());
        }
        let index = next_name_index;
        next_name_index += 1;
        names.insert(name.to_string(), index);
        encoded_names.extend_from_slice(&((name.len() + 1) as i32).to_le_bytes());
        encoded_names.extend_from_slice(name.as_bytes());
        encoded_names.push(0);
        encoded_names.extend_from_slice(&0u64.to_le_bytes());
        Ok(index)
    };
    for name in additional_names {
        name_index(name)?;
    }
    let exports = exported_names(&original, info)?;
    let mut paths = HashMap::new();
    for index in 0..info.import_count {
        paths.insert(
            object_path(&original, info, &exports, -(index as i32 + 1))?.to_ascii_lowercase(),
            -(index as i32 + 1),
        );
    }
    let table_start = info
        .import_offset
        .checked_sub(info.name_offset)
        .ok_or("Invalid import table")?;
    let mut table = original
        .get(table_start..table_start + info.import_count * 28)
        .ok_or("Truncated import table")?
        .to_vec();
    let mut count = info.import_count;
    for (path, class_package, class_name) in objects {
        let parts: Vec<_> = path.split('.').collect();
        if parts.len() < 2 {
            return Err("External asset requires a package-qualified path".into());
        }
        let mut parent = 0i32;
        for (i, part) in parts.iter().enumerate() {
            let prefix = parts[..=i].join(".").to_ascii_lowercase();
            if let Some(index) = paths.get(&prefix) {
                parent = *index;
                continue;
            }
            let leaf = i + 1 == parts.len();
            let package = name_index(if leaf { class_package } else { "Core" })?;
            let class = name_index(if leaf { class_name } else { "Package" })?;
            let object = name_index(part)?;
            for index in [package, class] {
                table.extend_from_slice(&(index as i32).to_le_bytes());
                table.extend_from_slice(&0i32.to_le_bytes());
            }
            table.extend_from_slice(&parent.to_le_bytes());
            table.extend_from_slice(&(object as i32).to_le_bytes());
            table.extend_from_slice(&0i32.to_le_bytes());
            count += 1;
            parent = -(count as i32);
            paths.insert(prefix, parent);
        }
    }
    drop(name_index);
    if count == info.import_count && encoded_names.is_empty() {
        if source != destination {
            fs::copy(source, destination).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    let delta = i32::try_from(encoded_names.len() + table.len())
        .map_err(|_| "Import table growth overflow")?;
    if let Some(rebased) = rebase_texture_bulk(source, delta)? {
        raw = rebased;
    }
    let (mut header, _, old_encoded) = find_key_and_decrypt(&raw, info, &candidates)?;
    let mut inserted = encoded_names.clone();
    inserted.extend_from_slice(&table);
    header.splice(insert..insert, inserted);
    let new_count = next_name_index;
    write_i32(&mut raw, 25, new_count as i32)?;
    if let Some(offset) = info.generation_name_count_offset {
        write_i32(&mut raw, offset, new_count as i32)?;
    }
    apply_deltas(&mut raw, &mut header, info, delta)?;
    write_i32(&mut raw, 41, count as i32)?;
    write_i32(
        &mut raw,
        45,
        (info.name_offset + insert + encoded_names.len()) as i32,
    )?;
    let (old_plain, _) = encrypted_size(info)?;
    let plain = old_plain
        .checked_add(delta as usize)
        .ok_or("Header size overflow")?;
    let encoded = if info.package_flags & 0x0800 != 0 {
        plain
    } else {
        plain.div_ceil(16) * 16
    };
    header.resize(encoded, 0);
    let encrypted = crypt_for_info(&header[..encoded], &key, true, info)?;
    let mut output = raw[..info.name_offset].to_vec();
    output.extend_from_slice(&encrypted);
    output.extend_from_slice(&raw[info.name_offset + old_encoded..]);
    fs::write(destination, output).map_err(|e| e.to_string())
}

/// Give each editable decal its own carrier package and package GUID.
pub fn create_decal_carrier(
    source: &Path,
    destination: &Path,
    base_dir: &Path,
    source_asset: &str,
    target_asset: &str,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let mut template = fs::read(source).map_err(|e| e.to_string())?;
    let info = read_info(&template)?;
    let guid = Sha256::digest(target_asset.as_bytes());
    template[info.package_guid_offset..info.package_guid_offset + 16].copy_from_slice(&guid[..16]);
    let temporary = destination.with_extension("carrier-template.tmp");
    fs::write(&temporary, template).map_err(|e| e.to_string())?;
    let result = patch_for_target(
        source,
        &temporary,
        destination,
        base_dir,
        Some(source_asset),
        Some(target_asset),
    );
    let _ = fs::remove_file(temporary);
    result
}
