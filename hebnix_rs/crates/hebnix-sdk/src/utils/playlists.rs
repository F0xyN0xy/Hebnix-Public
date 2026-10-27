//! Shared player-facing playlist names for UI, presence, logs and replays.
pub const PLAYLIST_NAMES: &[(i64, &str)] = &[
    (-2, "Intermission"),
    (0, "Casual"),
    (1, "Duel"),
    (2, "Doubles"),
    (3, "Standard"),
    (4, "Chaos"),
    (6, "Private Match"),
    (7, "Season"),
    (8, "Exhibition"),
    (9, "Training"),
    (10, "Duel (Ranked)"),
    (11, "Doubles (Ranked)"),
    (12, "Solo Standard"),
    (13, "Standard (Ranked)"),
    (15, "Snow Day"),
    (16, "Rocket Labs"),
    (17, "Hoops"),
    (18, "Rumble"),
    (19, "Workshop"),
    (20, "Custom Training Editor"),
    (21, "Custom Training"),
    (22, "Custom Tournament"),
    (23, "Dropshot"),
    (24, "Local Match"),
    (25, "Rumble"), // Retain the existing legacy alias.
    (26, "External Match (Ranked)"),
    (27, "Hoops (Ranked)"),
    (28, "Rumble (Ranked)"),
    (29, "Dropshot (Ranked)"),
    (30, "Snow Day (Ranked)"),
    (31, "Ghost Hunt"),
    (32, "Beach Ball"),
    (33, "Spike Rush"),
    (34, "Tournament Match"),
    (35, "Rocket Labs"),
    (37, "Dropshot Rumble"),
    (38, "Heatseeker"),
    (41, "Boomer Ball"),
    (43, "Heatseeker Doubles"),
    (44, "Winter Breakaway"),
    (46, "Gridiron"),
    (47, "Super Cube"),
    (48, "Tactical Rumble"),
    (49, "Spring Loaded"),
    (50, "Speed Demon"),
    (52, "Gotham City Rumble"),
    (54, "Knockout"),
    (55, "Confidential Third Wheel Test"),
    (61, "Quads (Ranked)"),
    (62, "Nike FC Showdown"),
    (63, "Heatseeker"),
    (64, "Haunted Heatseeker"),
    (65, "Haunted Heatseeker"),
    (66, "Heatseeker Ricochet"),
    (67, "Spooky Cube"),
    (68, "G-Force Frenzy"),
    (70, "Dropshot Rumble Doubles"),
    (72, "Split Shot"),
    (73, "Online Free Play"),
    (74, "Split Shot Doubles"),
    (75, "Split Shot Heatseeker"),
    (76, "Split Shot Heatseeker Doubles"),
    (77, "Non-Standard Soccar"),
    (79, "Split Shot Snow Day"),
    (80, "Run It Back"),
    (81, "Car Wars"),
    (82, "Pizza Party"),
    (83, "Push the Puck"),
    (84, "Possession Rumble"),
    (86, "Soccar Strike"),
    (87, "Sacrifice"),
    (88, "Jump Jam"),
    (92, "Bullet Ball Casual"),
];

pub fn known_name(id: i64) -> Option<&'static str> {
    PLAYLIST_NAMES
        .iter()
        .find(|(playlist, _)| *playlist == id)
        .map(|(_, name)| *name)
}

pub fn display_name(id: i64) -> String {
    known_name(id)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Playlist {id}"))
}
