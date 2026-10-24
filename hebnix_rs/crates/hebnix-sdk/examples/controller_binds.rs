//! Read-only controller binding inspection for an explicitly supplied save.
fn main() {
    let path = std::env::args_os().nth(1).expect("Pass a .save path");
    let path = std::path::Path::new(&path);
    let save = hebnix_sdk::save_file::load(path, true).expect("Cannot read save");
    println!("File: {}", path.display());
    if let Some(gamepad) = save.gamepad_bindings() {
        println!("Saved controller profile: {}", gamepad.raw);
    }
    for object in &save.objects {
        let Some(fields) = object.as_object() else { continue };
        for (name, value) in fields {
            if name.to_ascii_lowercase().contains("gamepadbinding") {
                println!("{} / {name}: {value}", object["__type"]);
            }
        }
    }
    for value in hebnix_sdk::save_file::list_configuration(path).expect("Cannot read settings") {
        if value.category == "Controller" || value.category == "Controller Bindings" {
            println!("{} / {}: {}", value.category, value.name, value.value);
        }
    }
}
