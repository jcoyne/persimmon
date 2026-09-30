# IIIF Image API validator

The fixture `fixtures/iiif-validator.jp2` is the [IIIF image validator's CC0 test image](https://iiif.io/api/image/validator/download/). Upload it as `<identifier>.jp2` to the configured source S3 bucket, then run the [IIIF image validator](https://github.com/IIIF/image-validator) against the public service URL:

```sh
iiif-validate.py --scheme=https -s images.example.edu -p iiif/3 \
  -i validator --version=3.0 --level=2 -v
```

The local run using the supplied JP2 fixture passed 31 of 33 checks with a development Kakadu executable. A second run against the Linux `amd64` Docker image built from the licensed Kakadu 8.6.2 SDK, using the native adapter and local Moto S3, also passed 31 of 33 checks. This does not validate the final release deployment or representative source images. The two remaining validator failures are:

- `quality_bitonal` expects 200 for bitonal output. Bitonal is optional at Level 2 in the [IIIF compliance table](https://iiif.io/api/image/3.0/compliance/), and this service intentionally does not advertise it.
- `size_noup` expects `!2000,3000` to fail for the 1000 by 1000 fixture. The [IIIF confined-size definition](https://iiif.io/api/image/3.0/#42-size) says the returned image must be no larger than the source region as well as the requested bounds, so the correct result is 1000 by 1000. The [validator test](https://github.com/IIIF/image-validator/blob/1740893f1fb22960142071a9f3d1c99122a190c7/iiif_validator/tests/size_noup.py#L12-L30) hardcodes this request as an upscaling failure without considering the source dimensions and raises an internal `TypeError` when it succeeds. Explicit `2000,3000` and unprefixed `pct:200` requests require upscaling and return HTTP 400; `^` upscaling is unsupported and returns HTTP 501.

Keep the full validator output when evaluating the release. Rerun with the chosen licensed Kakadu build (8.6 or newer) and representative color and grayscale JP2 images. The CLI needs `libmagic` installed; the initial local run used a temporary import shim only because the optional JP2 output format check is outside this service's supported formats.
