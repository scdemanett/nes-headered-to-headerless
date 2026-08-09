# NES Headered → Headerless Fixdat Converter

Find headered NES ROMs that match a RomVault **headerless** fixdat, strip the 16-byte iNES header, verify CRC/SHA1/MD5, and write `.unh` files ready for RomVault to fix.

Sources are never modified. Matching is CRC-first (not filename), so headered `.nes` files inside `.zip` or `.7z` archives work as long as the stripped payload matches the fixdat.

## Requirements

- [uv](https://docs.astral.sh/uv/)
- Python 3.10+

## Setup

```bash
uv sync
```

## Usage

```bash
uv run nes_fix.py --fixdat <fixdat.dat> --input <headered_dir> --output <out_dir>
```

### Options

| Flag | Description |
|------|-------------|
| `--fixdat` | RomVault headerless fixdat (Logiqx XML `.dat`) |
| `--input` | Directory of headered ROMs (loose `.nes`/`.unh`, `.zip`, `.7z`) |
| `--output` | Directory for verified headerless `.unh` files |
| `--dry-run` | List matches without writing files |

### Example

```powershell
uv run nes_fix.py `
  --fixdat "C:\Users\Steven\Desktop\fixDat_Nintendo_....dat" `
  --input "F:\(ROM Rebuilding)\Nintendo\...\Nintendo Entertainment System (Headered) (Aftermarket) (No-Intro)" `
  --output "F:\(ROM Rebuilding)\Nintendo\...\to-import-headerless"
```

After running, copy the output files into your headerless set (or point RomVault at the output folder) and run **Fix**.

## How matching works

1. Parse the fixdat and build a map of `(crc32, size)` → expected `.unh` name and hashes.
2. Recursively scan `--input` for `.nes`, `.unh`, `.zip`, and `.7z` files.
3. For each candidate, remove the iNES header when the file starts with `NES\x1a` (16 bytes only; trainer data is kept).
4. Verify CRC32, size, and SHA1/MD5 when present in the fixdat.
5. Write matching files to `--output` using the exact rom name from the fixdat.

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | All fixdat entries satisfied |
| `1` | Usage error, missing paths, or runtime failure |
| `2` | Some fixdat entries are still missing |
