# IIIF Image API validator

The fixture `fixtures/iiif-validator.jp2` is the [IIIF image validator's CC0 test image](https://iiif.io/api/image/validator/download/). Upload it as `<identifier>.jp2` to the configured source S3 bucket, then run the [IIIF image validator](https://github.com/IIIF/image-validator) against the public service URL:

```sh
iiif-validate.py --scheme=https -s images.example.edu -p iiif/3 \
  -i validator --version=3.0 --level=2 -v
```

On 2026-09-30, upstream validator commit [`f05e365a1cb28f8707cbd50466a86d8f611d1ab8`](https://github.com/IIIF/image-validator/commit/f05e365a1cb28f8707cbd50466a86d8f611d1ab8) passed **31 of 31 Level 2 checks, with zero failures** against the Linux `amd64` `persimmon:dev` Docker image built with Kakadu 8.6.2 and its native adapter. The image read the fixture from local Moto S3 over native TLS at `https://127.0.0.1:3005/v3/validator`. The Docker image ID was `sha256:4d52d9198838e295f5a75835a2cc8b7118c7bf8d0fa63db5556973041ab775af`. The complete CLI output is in [validator-results-2026-09-30.log](validator-results-2026-09-30.log).

The earlier run against validator commit `1740893` passed 31 of 33 checks. Upstream now treats `quality_bitonal` as optional for v3 Level 2, as required by the [IIIF compliance table](https://iiif.io/api/image/3.0/compliance/). It also accepts `!2000,3000` for the 1000 by 1000 fixture in `size_noup`: under the [confined-size rule](https://iiif.io/api/image/3.0/#42-size), that request must not upscale the image. Both changes explain the new result; no Persimmon code change was needed.

The run used `--scheme=https -s 127.0.0.1:3005 -p v3 -i validator --version=3.0 --level=2 -v`. Python loaded the pulled upstream checkout directly through `PYTHONPATH`. A temporary `magic` import shim allowed the validator to load optional JP2/PDF output-format tests without system `libmagic`; those tests are outside the v3 Level 2 selection and were not executed. The test used one small color JP2 and local Moto, so it does not validate representative archival color/grayscale images, AWS S3, sustained load, or the eventual release build. Rerun those checks before claiming production Level 2 compliance.
