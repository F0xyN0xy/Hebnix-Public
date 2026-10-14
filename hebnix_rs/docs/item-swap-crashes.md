# Item swap package relocation

The local game log `Launch-backup-2026.10.11-15.25.51.log` records a fatal
`Bad name index -1/548` while loading `Boost_Standard_Yellow_SF.upk`.
The active swap replaces Standard Yellow with Gold Rush.

Renaming Gold Rush to Standard Yellow increases the name table by eight bytes.
Previously the encrypted header's end moved from 43867 to 43875, but the first
compressed chunk and its export data still started at logical address 43867.
Rocket League's compression/decryption reader therefore treated eight bytes of
object data as header data. The application's package reader overlaid the chunks
directly and did not catch the inconsistent boundary. The same failure applies
to other names that grow or shrink, including trail and thumbnail swaps.

The fix shifts export serial addresses and every chunk's uncompressed address
by the name-table delta. Physical compressed addresses move independently by
the AES alignment delta. Inline texture bulk addresses are rebased; external
texture-cache addresses and pixel data are preserved. CTR header bounds use the
last-block field, matching the existing package reader. A target key must be
verified instead of falling back to the donor's key.

Validation now requires agreement between the compression boundary and the
summary's data boundary, rejects exports inside the header, and checks the full
target object path. The main asset can be reparented to an existing target
package group; missing groups are rejected before installation.

Thumbnail declarations are read from the pristine packages. This handles both
different filename/object names and shared packages such as `SS_Tier_01_T`,
where the Bronze and Silver thumbnails must be selected using the main asset.
Resolution errors use the existing rollback path.

Verification uses staged output under `target/swap-identity-check`, without
changing installed game packages. The example tests both directions, pristine
`.bak` donors, all 13 item categories, Binary and Standard Yellow replacements,
and their thumbnails. It checks uniform export relocation and byte preservation
for the installed fixtures. Unit tests cover growing/shrinking names, AES
alignment, CTR bounds, object hierarchy, thumbnail identity, and cycles.

On 13 October 2026, all 115 staged swaps and all nine regression tests passed.
The full application also passed `cargo check` and the release executable built
successfully at `target/release/hebnix.exe`.

```powershell
cargo test -p hebnix-app --example swap_identity_check --offline cosmetic_upk::tests
cargo run -p hebnix-app --example swap_identity_check --offline -- '<CookedPCConsole>' '<catalogs>'
```

These are package-level checks. An in-game equip/boost/trail/match test is still
needed to confirm runtime behavior. Existing swaps retain the old bytes until
restored and reapplied with the updated executable.

Reader reference: [UEViewer's Rocket League loader](https://github.com/gildor2/UEViewer/blob/master/Unreal/UnrealPackage/UnPackageReader.cpp).
