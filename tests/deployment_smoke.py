#!/usr/bin/env python3
"""Read-only smoke check for one deployed IIIF v3 image service.

Pass the service id (the image URL before /full/...) as --service-url.
This checks the public route and one small derivative in each output format.
It does not test S3 permissions, purge propagation, cleanup, or load.
"""

import argparse
import json
import ssl
import urllib.error
import urllib.request


def request(opener, url, method="GET", headers=None):
    req = urllib.request.Request(url, method=method, headers=headers or {})
    try:
        with opener.open(req, timeout=30) as response:
            return response.status, response.headers, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read()


def check(condition, message):
    if not condition:
        raise AssertionError(message)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--service-url", required=True, help="IIIF service id, without /info.json")
    parser.add_argument("--insecure", action="store_true", help="accept a self-signed test certificate")
    parser.add_argument("--no-proxy", action="store_true", help="ignore proxy settings")
    args = parser.parse_args()
    service = args.service_url.rstrip("/")
    check(service.startswith("https://") or service.startswith("http://"), "service URL needs http(s)")
    context = ssl._create_unverified_context() if args.insecure else ssl.create_default_context()
    handlers = [urllib.request.HTTPSHandler(context=context), NoRedirect()]
    if args.no_proxy:
        handlers.append(urllib.request.ProxyHandler({}))
    opener = urllib.request.build_opener(*handlers)

    status, headers, body = request(opener, service)
    check(status == 303, f"base route: expected 303, got {status}")
    check(headers.get("Location") == service + "/info.json", "base route Location differs from info URL")

    status, headers, body = request(opener, service + "/info.json")
    check(status == 200, f"info.json: HTTP {status}")
    check(headers.get_content_type() == "application/ld+json", "info.json: expected JSON-LD")
    check("Accept" in headers.get("Vary", ""), "info.json: missing Vary: Accept")
    info = json.loads(body)
    check(info.get("id") == service, f"info.json id differs: {info.get('id')}")
    check(info.get("type") == "ImageService3", "info.json: unexpected type")
    check(info.get("width", 0) > 0 and info.get("height", 0) > 0, "info.json: invalid dimensions")

    status, headers, body = request(opener, service + "/info.json", headers={"Accept": "application/json"})
    check(status == 200 and headers.get_content_type() == "application/json", "JSON negotiation failed")
    check(json.loads(body)["id"] == service, "JSON info id differs")

    image_root = service + "/full/128,/0/default"
    for extension, mime, magic in [
        ("jpg", "image/jpeg", b"\xff\xd8"),
        ("png", "image/png", b"\x89PNG\r\n\x1a\n"),
        ("webp", "image/webp", b"RIFF"),
        ("avif", "image/avif", b"ftypavif"),
    ]:
        url = image_root + "." + extension
        status, headers, body = request(opener, url)
        check(status == 200, f"{extension}: HTTP {status}")
        check(headers.get_content_type() == mime, f"{extension}: wrong Content-Type")
        if extension == "avif":
            check(body[4:12] == magic, "avif: wrong image signature")
        else:
            check(body.startswith(magic), f"{extension}: wrong image signature")
        check(len(body) == int(headers.get("Content-Length", "-1")), f"{extension}: wrong Content-Length")
        check("max-age=86400" in headers.get("Cache-Control", ""), f"{extension}: wrong cache policy")
        if extension == "webp":
            check(body[8:12] == b"WEBP", "webp: wrong image signature")
        status, head_headers, head_body = request(opener, url, method="HEAD")
        check(status == 200 and not head_body, f"{extension}: HEAD failed")
        check(head_headers.get("Content-Length") == headers.get("Content-Length"),
              f"{extension}: HEAD Content-Length differs")

    status, headers, body = request(opener, image_root + ".jpg", method="OPTIONS",
                                    headers={"Origin": "https://example.org",
                                             "Access-Control-Request-Method": "GET"})
    check(200 <= status < 300, f"CORS preflight: HTTP {status}")
    check(headers.get("Access-Control-Allow-Origin") in ("*", "https://example.org"),
          "CORS preflight: missing allowed origin")

    invalid_url = service.rsplit("/", 1)[0] + "/a%2F..%2Fb/info.json"
    status, headers, body = request(opener, invalid_url)
    check(status == 400, f"invalid identifier: expected HTTP 400, got {status}")

    print(f"PASS: {service}: redirect, info, JPEG/PNG/WebP/AVIF, HEAD, cache headers, CORS, invalid identifier")
    print(f"Image dimensions: {info['width']} x {info['height']}")


if __name__ == "__main__":
    main()
