#!/usr/bin/env python3
"""Verify that a IIIF Image API service delivers its advertised sizes and tiles.

Pass the service id from info.json as --service-url. This tool streams JPEG
responses through a small buffer, so large images do not fill client memory.
"""

import argparse
import json
import math
import ssl
import urllib.request


SOF_MARKERS = {0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7,
               0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF}


def jpeg_dimensions(response):
    """Return (width, height, bytes_consumed) after reading through the SOF."""
    count = 0

    def read(size):
        nonlocal count
        data = response.read(size)
        if len(data) != size:
            raise ValueError("truncated JPEG")
        count += size
        return data

    if read(2) != b"\xff\xd8":
        raise ValueError("response is not a JPEG")
    while True:
        if read(1) != b"\xff":
            raise ValueError("invalid JPEG marker")
        marker = read(1)[0]
        while marker == 0xFF:
            marker = read(1)[0]
        if marker in (0xD8, 0xD9) or 0xD0 <= marker <= 0xD7:
            continue
        length = int.from_bytes(read(2), "big")
        if length < 2:
            raise ValueError("invalid JPEG segment length")
        payload = read(length - 2)
        if marker in SOF_MARKERS:
            if len(payload) < 5:
                raise ValueError("invalid JPEG frame header")
            height = int.from_bytes(payload[1:3], "big")
            width = int.from_bytes(payload[3:5], "big")
            return width, height, count


def verify_jpeg(opener, url, expected):
    with opener.open(url, timeout=120) as response:
        if response.status != 200 or response.headers.get_content_type() != "image/jpeg":
            raise AssertionError(f"{url}: HTTP {response.status}, {response.headers.get_content_type()}")
        width, height, size = jpeg_dimensions(response)
        if (width, height) != expected:
            raise AssertionError(f"{url}: expected {expected}, got {(width, height)}")
        while chunk := response.read(1024 * 1024):
            size += len(chunk)
        content_length = response.headers.get("Content-Length")
        if content_length is not None and int(content_length) != size:
            raise AssertionError(f"{url}: Content-Length {content_length}, received {size}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--service-url", required=True, help="IIIF service id, without /info.json")
    parser.add_argument("--insecure", action="store_true", help="accept a self-signed test certificate")
    parser.add_argument("--no-proxy", action="store_true", help="connect directly, ignoring proxy settings")
    args = parser.parse_args()
    service_url = args.service_url.rstrip("/")
    context = ssl._create_unverified_context() if args.insecure else ssl.create_default_context()
    handlers = [urllib.request.HTTPSHandler(context=context)]
    if args.no_proxy:
        handlers.append(urllib.request.ProxyHandler({}))
    opener = urllib.request.build_opener(*handlers)
    with opener.open(service_url + "/info.json", timeout=30) as response:
        info = json.load(response)
    if info["id"] != service_url:
        raise AssertionError(f"info.json id {info['id']} differs from {service_url}")
    width, height = info["width"], info["height"]
    sizes = info.get("sizes", [])
    for size in sizes:
        w, h = size["width"], size["height"]
        verify_jpeg(opener, f"{service_url}/full/{w},{h}/0/default.jpg", (w, h))

    tested_tiles = 0
    for tile in info.get("tiles", []):
        tile_width = tile["width"]
        tile_height = tile.get("height", tile_width)
        for factor in tile["scaleFactors"]:
            for x, y in {(0, 0), (
                ((width - 1) // (tile_width * factor)) * tile_width * factor,
                ((height - 1) // (tile_height * factor)) * tile_height * factor,
            )}:
                region_width = min(tile_width * factor, width - x)
                region_height = min(tile_height * factor, height - y)
                output = (math.ceil(region_width / factor), math.ceil(region_height / factor))
                url = (f"{service_url}/{x},{y},{region_width},{region_height}/"
                       f"{output[0]},{output[1]}/0/default.jpg")
                verify_jpeg(opener, url, output)
                tested_tiles += 1
    print(f"PASS: {service_url}: {len(sizes)} sizes, {tested_tiles} tile requests")


if __name__ == "__main__":
    main()
