#!/usr/bin/env python3
"""Create a synthetic grayscale or RGB JP2 fixture with a licensed kdu_compress.

Random pixels make the encoded file size track the requested bit rate. The
temporary PPM is deleted after compression, even when compression fails.
"""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--width", type=int, required=True)
    parser.add_argument("--height", type=int, required=True)
    parser.add_argument("--rate", type=float, required=True, help="target bits per pixel")
    parser.add_argument("--channels", type=int, choices=(1, 3), default=3)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--compressor", default="kdu_compress")
    parser.add_argument("--workdir", type=Path, help="working directory required by some Kakadu builds")
    parser.add_argument("--threads", type=int, default=8)
    args = parser.parse_args()
    if min(args.width, args.height, args.threads) <= 0 or not 0 < args.rate <= 8 * args.channels:
        parser.error("width, height, threads, and rate must be positive; rate must fit the channel depth")
    if args.output.suffix.lower() != ".jp2":
        parser.error("output must have a .jp2 suffix")
    args.output = args.output.resolve()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="wb", suffix=".pgm" if args.channels == 1 else ".ppm",
        prefix="persimmon-synthetic-", dir=args.output.parent, delete=False
    ) as source:
        source_path = Path(source.name)
        magic = "P5" if args.channels == 1 else "P6"
        source.write(f"{magic}\n{args.width} {args.height}\n255\n".encode("ascii"))
        remaining = args.width * args.height * args.channels
        while remaining:
            chunk = os.urandom(min(remaining, 1024 * 1024))
            source.write(chunk)
            remaining -= len(chunk)
    try:
        subprocess.run(
            [
                args.compressor,
                "-i", str(source_path),
                "-o", str(args.output),
                "-rate", str(args.rate),
                "-num_threads", str(args.threads),
            ],
            cwd=args.workdir,
            check=True,
        )
    finally:
        source_path.unlink(missing_ok=True)
    print(f"{args.output}: {args.output.stat().st_size} bytes")


if __name__ == "__main__":
    main()
