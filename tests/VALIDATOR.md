# IIIF Image API validator

The fixture `fixtures/iiif-validator.jp2` is the [IIIF image validator's CC0 test image](https://iiif.io/api/image/validator/download/). Upload it under the exact key `validator` in the configured source S3 bucket; Persimmon does not add `.jp2` to an identifier.

`iiif-validate.py` belongs to the [upstream IIIF image-validator repository](https://github.com/IIIF/image-validator), not Persimmon. The local checkout is at `/Users/jcoyne85/workspace/jcoyne/image-validator`, currently revision `b9b05a02a1ae19d30ad176d4026a3c51973e231e`. That is one commit newer than the `f05e365a1cb28f8707cbd50466a86d8f611d1ab8` revision used for the local baseline; it updates Bottle to 0.13. From the Persimmon repository root, prepare a Python 3.12 virtual environment without writing into the sibling checkout:

```sh
cd /Users/jcoyne85/workspace/jcoyne/persimmon
git -C ../image-validator rev-parse HEAD
/opt/homebrew/bin/python3.12 -m venv /tmp/iiif-validator-sibling-venv
/tmp/iiif-validator-sibling-venv/bin/python -m pip install \
  'bottle>=0.13.4,<0.14' 'lxml>=3.7.0' 'Pillow>=6.2.2' 'python-magic>=0.4.12'
```

The virtual environment above is already prepared on this workstation, and the CLI help command loads successfully. Use Python 3.12 because the default `python3` here is 3.14. The validator also imports its optional JP2/PDF output tests at startup, which require system `libmagic`; these are outside Image API 3.0 Level 2. The command below uses a local import stub that fails if either optional test actually tries to use it. For Level 3 tests, install `libmagic` instead and remove the stub from `PYTHONPATH`.

Run against the deployed fixture from the Persimmon repository root:

```sh
PYTHONPATH=tests/validator_support:/Users/jcoyne85/workspace/jcoyne/image-validator \
  /tmp/iiif-validator-sibling-venv/bin/python \
  /Users/jcoyne85/workspace/jcoyne/image-validator/iiif-validate.py --scheme=https \
  -s sul-imageserver-test-a.stanford.edu -p v3 \
  -i validator --version=3.0 --level=2 -v
```

On 2026-10-06, the operator ran this command against the reported deployed
Persimmon revision `7db27dc4246e`. The sibling validator checkout was at
`b9b05a02a1ae19d30ad176d4026a3c51973e231e`. The output ended with
**`Done (31 tests, 0 failures)`**. The selected tests all passed:

| Tests | Result |
| --- | --- |
| `baseurl_redirect`, `cors`, `format_error_random`, `format_jpg`, `format_png` | PASS |
| `id_basic`, `id_error_escapedslash`, `id_error_random`, `id_error_unescaped`, `id_escaped`, `id_squares` | PASS |
| `info_json`, `jsonld`, `quality_color`, `quality_error_random` | PASS |
| `region_error_random`, `region_percent`, `region_pixels`, `region_square` | PASS |
| `rot_error_random`, `rot_full_basic`, `rot_region_basic` | PASS |
| `size_bwh`, `size_ch`, `size_error_random`, `size_nofull`, `size_noup`, `size_percent`, `size_region`, `size_wc`, `size_wh` | PASS |

This verifies selected Level 2 behavior of the deployed build with the CC0
fixture. It does not test representative archival component layouts, grayscale
sources, native decoder use, concurrency, or AWS failure modes. The full CLI
output was supplied in the conversation; it has not been saved in this repo.

On 2026-09-30, upstream validator commit [`f05e365a1cb28f8707cbd50466a86d8f611d1ab8`](https://github.com/IIIF/image-validator/commit/f05e365a1cb28f8707cbd50466a86d8f611d1ab8) passed **31 of 31 Level 2 checks, with zero failures** against the Linux `amd64` `persimmon:dev` Docker image built with Kakadu 8.6.2 and its native adapter. The image read the fixture from local Moto S3 over native TLS at `https://127.0.0.1:3005/v3/validator`. The Docker image ID was `sha256:4d52d9198838e295f5a75835a2cc8b7118c7bf8d0fa63db5556973041ab775af`. The complete CLI output is in [validator-results-2026-09-30.log](validator-results-2026-09-30.log).

The earlier run against validator commit `1740893` passed 31 of 33 checks. Upstream now treats `quality_bitonal` as optional for v3 Level 2, as required by the [IIIF compliance table](https://iiif.io/api/image/3.0/compliance/). It also accepts `!2000,3000` for the 1000 by 1000 fixture in `size_noup`: under the [confined-size rule](https://iiif.io/api/image/3.0/#42-size), that request must not upscale the image. Both changes explain the new result; no Persimmon code change was needed.

The run used `--scheme=https -s 127.0.0.1:3005 -p v3 -i validator --version=3.0 --level=2 -v`. Python loaded the pulled upstream checkout directly through `PYTHONPATH`. A temporary `magic` import shim allowed the validator to load optional JP2/PDF output-format tests without system `libmagic`; those tests are outside the v3 Level 2 selection and were not executed. The test used one small color JP2 and local Moto, so it does not validate representative archival color/grayscale images, AWS S3, sustained load, or the eventual release build. Rerun those checks before claiming production Level 2 compliance.
