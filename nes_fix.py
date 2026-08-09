#!/usr/bin/env python3
"""Convert headered NES ROMs to headerless .unh files matching a RomVault fixdat."""

from __future__ import annotations

import argparse
import hashlib
import io
import sys
import tempfile
import xml.etree.ElementTree as ET
import zipfile
import zlib
from dataclasses import dataclass
from pathlib import Path

INES_MAGIC = b"NES\x1a"
INES_HEADER_SIZE = 16
ROM_EXTENSIONS = {".nes", ".unh"}
ARCHIVE_EXTENSIONS = {".zip", ".7z"}


@dataclass(frozen=True)
class RomEntry:
    game_name: str
    rom_name: str
    size: int
    crc: str
    sha1: str | None
    md5: str | None


@dataclass
class CandidateMatch:
    entry: RomEntry
    source: str


def parse_fixdat(path: Path) -> dict[tuple[str, int], RomEntry]:
    """Parse fixdat XML into a map keyed by (crc, size)."""
    tree = ET.parse(path)
    root = tree.getroot()
    entries: dict[tuple[str, int], RomEntry] = {}

    for game in root.findall("game"):
        game_name = game.get("name", "")
        for rom in game.findall("rom"):
            crc = (rom.get("crc") or "").lower()
            size_str = rom.get("size")
            if not crc or not size_str:
                continue
            size = int(size_str)
            key = (crc, size)
            entries[key] = RomEntry(
                game_name=game_name,
                rom_name=rom.get("name", ""),
                size=size,
                crc=crc,
                sha1=(rom.get("sha1") or "").lower() or None,
                md5=(rom.get("md5") or "").lower() or None,
            )

    return entries


def strip_ines_header(data: bytes) -> bytes:
    """Remove the 16-byte iNES header when present."""
    if len(data) >= INES_HEADER_SIZE and data[:4] == INES_MAGIC:
        return data[INES_HEADER_SIZE:]
    return data


def compute_hashes(data: bytes) -> tuple[str, str, str]:
    crc = format(zlib.crc32(data) & 0xFFFFFFFF, "08x")
    sha1 = hashlib.sha1(data).hexdigest()
    md5 = hashlib.md5(data).hexdigest()
    return crc, sha1, md5


def verify_payload(data: bytes, entry: RomEntry) -> bool:
    crc, sha1, md5 = compute_hashes(data)
    if crc != entry.crc or len(data) != entry.size:
        return False
    if entry.sha1 and sha1 != entry.sha1:
        return False
    if entry.md5 and md5 != entry.md5:
        return False
    return True


def file_matches_entry(path: Path, entry: RomEntry) -> bool:
    return verify_payload(strip_ines_header(path.read_bytes()), entry)


def iter_rom_members_from_zip(path: Path) -> list[tuple[str, bytes]]:
    members: list[tuple[str, bytes]] = []
    with zipfile.ZipFile(path, "r") as archive:
        for info in archive.infolist():
            if info.is_dir():
                continue
            member_name = info.filename
            suffix = Path(member_name).suffix.lower()
            if suffix not in ROM_EXTENSIONS:
                continue
            members.append((member_name, archive.read(info)))
    return members


def iter_rom_members_from_7z(path: Path) -> list[tuple[str, bytes]]:
    try:
        import py7zr
    except ImportError as exc:
        raise RuntimeError(
            "py7zr is required to read .7z archives. Install with: uv sync"
        ) from exc

    members: list[tuple[str, bytes]] = []
    with py7zr.SevenZipFile(path, mode="r") as archive:
        targets = [
            name
            for name in archive.getnames()
            if Path(name).suffix.lower() in ROM_EXTENSIONS
        ]
        if not targets:
            return members
        extracted = archive.read(targets)
        for name in targets:
            payload = extracted[name]
            if isinstance(payload, io.BytesIO):
                members.append((name, payload.getvalue()))
            else:
                members.append((name, payload))
    return members


def iter_candidates(input_dir: Path) -> list[tuple[str, bytes]]:
    """Yield (source_label, raw_bytes) for every ROM candidate under input_dir."""
    candidates: list[tuple[str, bytes]] = []

    for path in sorted(input_dir.rglob("*")):
        if not path.is_file():
            continue

        suffix = path.suffix.lower()
        if suffix in ROM_EXTENSIONS:
            candidates.append((str(path), path.read_bytes()))
        elif suffix == ".zip":
            for member_name, payload in iter_rom_members_from_zip(path):
                candidates.append((f"{path}::{member_name}", payload))
        elif suffix == ".7z":
            for member_name, payload in iter_rom_members_from_7z(path):
                candidates.append((f"{path}::{member_name}", payload))

    return candidates


def atomic_write_bytes(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        dir=path.parent, delete=False, prefix=f".{path.name}.", suffix=".tmp"
    ) as tmp:
        tmp.write(data)
        tmp_path = Path(tmp.name)
    tmp_path.replace(path)


def process(
    fixdat_path: Path,
    input_dir: Path,
    output_dir: Path,
    dry_run: bool = False,
) -> int:
    needed = parse_fixdat(fixdat_path)
    if not needed:
        print("No ROM entries found in fixdat.", file=sys.stderr)
        return 1

    output_dir.mkdir(parents=True, exist_ok=True)

    remaining = dict(needed)
    written: list[CandidateMatch] = []
    skipped: list[CandidateMatch] = []
    unmatched_sources: list[str] = []

    for source, raw in iter_candidates(input_dir):
        payload = strip_ines_header(raw)
        if not payload:
            continue

        crc, _, _ = compute_hashes(payload)
        size = len(payload)
        key = (crc, size)
        entry = remaining.get(key)
        if entry is None:
            if crc in {e.crc for e in needed.values()}:
                unmatched_sources.append(f"{source} (CRC match, size mismatch: {size})")
            continue

        if not verify_payload(payload, entry):
            unmatched_sources.append(f"{source} (failed secondary hash check)")
            continue

        dest = output_dir / entry.rom_name
        if dest.exists() and file_matches_entry(dest, entry):
            skipped.append(CandidateMatch(entry=entry, source=source))
            del remaining[key]
            continue

        if dry_run:
            print(f"[dry-run] would write {dest.name} <= {source}")
        else:
            atomic_write_bytes(dest, payload)
            print(f"Wrote {dest.name} <= {source}")

        written.append(CandidateMatch(entry=entry, source=source))
        del remaining[key]

    print()
    print("Summary")
    print("-------")
    print(f"Needed from fixdat: {len(needed)}")
    print(f"Written:            {len(written)}")
    print(f"Skipped (present):  {len(skipped)}")
    print(f"Still missing:      {len(remaining)}")

    if remaining:
        print()
        print("Missing ROMs:")
        for entry in remaining.values():
            print(f"  - {entry.rom_name} (crc={entry.crc}, size={entry.size})")

    if unmatched_sources:
        print()
        print("Unmatched candidates (not written):")
        for line in unmatched_sources[:20]:
            print(f"  - {line}")
        if len(unmatched_sources) > 20:
            print(f"  ... and {len(unmatched_sources) - 20} more")

    return 0 if not remaining else 2


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Find headered NES ROMs matching a RomVault headerless fixdat, "
            "strip the iNES header, and write verified .unh files for RomVault."
        )
    )
    parser.add_argument(
        "--fixdat",
        required=True,
        type=Path,
        help="Path to the RomVault headerless fixdat (.dat)",
    )
    parser.add_argument(
        "--input",
        required=True,
        type=Path,
        help="Directory containing headered ROMs (loose, zip, or 7z)",
    )
    parser.add_argument(
        "--output",
        required=True,
        type=Path,
        help="Directory to write verified headerless .unh files",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Show what would be written without creating files",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)

    fixdat_path: Path = args.fixdat
    input_dir: Path = args.input
    output_dir: Path = args.output

    if not fixdat_path.is_file():
        print(f"Fixdat not found: {fixdat_path}", file=sys.stderr)
        return 1
    if not input_dir.is_dir():
        print(f"Input directory not found: {input_dir}", file=sys.stderr)
        return 1

    try:
        return process(fixdat_path, input_dir, output_dir, dry_run=args.dry_run)
    except RuntimeError as exc:
        print(str(exc), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
