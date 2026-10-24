//! Read-only diagnostics for save discovery and the readers used by plugins.
//! Run with an optional DBE_Production directory or individual .save path.

use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    use hebnix_sdk::save_file;

    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(save_file::detect_save_data_path);
    let Some(path) = path else {
        eprintln!(
            "No account save directory was detected. Pass the full DBE_Production directory as an argument."
        );
        return std::process::ExitCode::FAILURE;
    };
    let mut paths = if path.is_dir() {
        println!("Save directory: {}", path.display());
        let accounts = save_file::find_save_accounts(Some(&path));
        println!("Accounts visible to plugins: {}", accounts.len());
        if let Some(latest) = accounts.first() {
            println!("Default account: {}", latest.display_name);
            println!("Default save: {}", latest.path.display());
        }
        match std::fs::read_dir(&path) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file()
                        && path
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("save"))
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                eprintln!("Cannot list directory: {error}");
                return std::process::ExitCode::FAILURE;
            }
        }
    } else {
        vec![path]
    };
    paths.sort();
    if paths.is_empty() {
        eprintln!("No .save files found in this directory.");
        return std::process::ExitCode::FAILURE;
    }
    let mut failed = false;
    for path in paths {
        println!("\nFile: {}", path.display());
        match save_file::parse_savedata(&path, true) {
            Ok(save) => {
                println!(
                    "Summary reader: OK ({} objects; CRC {})",
                    save.objects.len(),
                    if save.crc_match { "OK" } else { "mismatch" }
                );
                for object in &save.objects {
                    if let Some(error) = object
                        .get("__parse_error")
                        .or_else(|| object.get("__error"))
                    {
                        println!("Object {}: {error}", object["__type"]);
                        failed = true;
                    }
                }
            }
            Err(error) => {
                println!("Summary reader: {error}");
                failed = true;
            }
        }
        match save_file::list_configuration(&path) {
            Ok(values) => {
                println!("Configuration reader: OK ({} settings)", values.len());
                let mut categories = std::collections::BTreeMap::new();
                for value in values {
                    *categories.entry(value.category).or_insert(0) += 1;
                }
                for (category, count) in categories {
                    println!("  {category}: {count}");
                }
            }
            Err(error) => {
                println!("Configuration reader: {error}");
                failed = true;
            }
        }
    }
    if failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}
