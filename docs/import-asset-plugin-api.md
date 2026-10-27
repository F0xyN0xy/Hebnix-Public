# Importing assets from a plugin

`hebnix.import_asset_async(req_id, opts)` copies a file into the plugin's own
`assets/` folder, so a plugin can offer an "Import image" button instead of
asking people to find the folder and refresh a list.

Without `opts.path` it opens a file picker. The call returns right away; the
result arrives in the plugin's `on_asset_imported` callback:

```lua
function plugin.on_asset_imported(req_id, name, error)
    if error then
        hebnix.log("import failed: " .. error)
    elseif name then
        -- name is the file name inside assets/, the same form
        -- hebnix.list_assets() returns
    else
        -- the picker was closed without choosing a file
    end
end

hebnix.import_asset_async("avatar", {
    title = "Choose an avatar",
    filter_name = "Images",
    extensions = { "png", "jpg", "jpeg", "gif" },
})
```

Options (all optional):

| key | meaning |
| --- | --- |
| `title` | file picker title |
| `filter_name` | label of the file type filter |
| `extensions` | allowed extensions; files with any other extension are rejected |
| `path` | copy this file instead of opening a picker |
| `name` | file name to save it as (only the last path part is used) |

If a different file already has that name, the copy is saved as
`name (2).ext`, `name (3).ext` and so on. Importing a file that is already
there with the same contents returns the existing name. Files over 64 MB are
rejected.
