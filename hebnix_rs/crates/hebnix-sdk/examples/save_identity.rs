//! Inspect stored identity fields without modifying the save or querying an API.
use serde_json::Value;

fn inspect(value: &Value, path: &str, owner_id: &str) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let field_path = format!("{path}.{key}");
                if let Some(text) = value.as_str() {
                    let lower = key.to_ascii_lowercase();
                    if matches!(lower.as_str(), "profilename" | "username" | "playername" | "displayname" | "epicusername" | "epicdisplayname" | "creatorname") {
                        println!("NAME {field_path}: {text:?}");
                    }
                    if !owner_id.is_empty() && text.to_ascii_lowercase().contains(owner_id) {
                        println!("OWNER ID {field_path}: {text:?}");
                        println!("OWNER OBJECT {path}: {value}", value = Value::Object(fields.clone()));
                    }
                }
                inspect(value, &field_path, owner_id);
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                inspect(value, &format!("{path}[{index}]"), owner_id);
            }
        }
        _ => {}
    }
}

fn main() {
    let path = std::env::args_os().nth(1).expect("Pass a .save path");
    let path = std::path::Path::new(&path);
    let owner = path.file_stem().unwrap().to_string_lossy();
    let owner = owner.split('_').next().unwrap().to_ascii_lowercase();
    let save = hebnix_sdk::save_file::load(path, true).expect("Cannot read save");
    println!("PROFILE: {:?}", save.profile().map(|profile| profile.profile_name));
    inspect(&save.raw_properties, "root", &owner);
    for (index, object) in save.objects.iter().enumerate() {
        let kind = object["__type"].as_str().unwrap_or("unknown");
        if object["CreatorPlayerID"]["EpicAccountId"]
            .as_str()
            .is_some_and(|id| id.eq_ignore_ascii_case(&owner))
        {
            println!("MATCHED OWNER: type={kind}; CreatorName={}; EpicAccountId={}",
                object["CreatorName"], object["CreatorPlayerID"]["EpicAccountId"]);
        }
        inspect(object, &format!("objects[{index}]({kind})"), &owner);
    }
}
