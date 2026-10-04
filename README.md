# NES Headerless Fixer

Drop a RomVault **headerless** fixdat, get verified `.unh` files. The app finds headered NES ROMs that match the fixdat, strips the 16-byte iNES header, verifies CRC/SHA1/MD5, and writes the results to your output folder, ready for RomVault to fix.

Search folders are only ever read. Matching is by hash, not filename, so headered `.nes` files inside `.zip` or `.7z` archives work as long as the stripped payload matches the fixdat.

## Using the app

1. Add one or more **search folders** (headered ROMs: loose `.nes`/`.unh`, `.zip`, `.7z`). You can also drop folders onto the window.
2. Choose one **output folder**.
3. Drop fixdats onto the window. They run immediately; several at once is fine.

Both settings are saved, so from then on it is just the drop. You can also drop a fixdat onto the exe or a shortcut to it: the app opens (or the running window takes over) and runs it against the saved folders.

Afterwards, point RomVault at the output folder (or copy the files into your headerless set) and run **Fix**.

### Speed

The first run hashes every ROM in the search folders and saves an index. Later runs only re-read files whose size or modified time changed, so a drop is close to instant. **Rebuild index** throws the index away if you ever want a full re-read. Every file is still verified against the fixdat's hashes before it is written.

### Settings and portable mode

Settings live in `%APPDATA%\com.scdemanett.nes-headerless-fixer\settings.json`:

```json
{
  "searchDirs": ["D:\\ROMs\\NES-headered", "E:\\More\\NES"],
  "outputDir": "D:\\ROMs\\NES-headerless-fixes",
  "dryRun": false
}
```

Put a `settings.json` next to the exe and the app runs portable: it uses that file and keeps its index beside it.

## Building

Requires [Node.js](https://nodejs.org/) and [Rust](https://rustup.rs/).

```bash
npm install
```

```bash
npm run dev
```

```bash
npm run package
```

`npm run package` produces the standalone exe at `src-tauri/target/release/nes-headerless-fixer.exe` and an installer under `src-tauri/target/release/bundle/nsis/`.

```bash
npm test
```

## How matching works

1. Recursively scan the search folders for `.nes`, `.unh`, `.zip`, and `.7z` files.
2. For each ROM, remove the iNES header when the file starts with `NES\x1a` (16 bytes only; trainer data is kept) and record the payload's CRC32 and size.
3. Parse the fixdat (Logiqx XML) and look up each entry by `(crc32, size)`.
4. Re-read each match, verify CRC32, size, and SHA1/MD5 when present in the fixdat.
5. Write it to the output folder using the exact ROM name from the fixdat. Files already there and correct are left alone.

## Python CLI

The original command-line script is still here and does the same conversion for a single input folder, without the index.

```bash
uv sync
```

```bash
uv run nes_fix.py --fixdat <fixdat.dat> --input <headered_dir> --output <out_dir>
```

Add `--dry-run` to list matches without writing files. Exit code `0` means every fixdat entry was satisfied, `2` means some are still missing, `1` is a usage or runtime error.
