"""Import stub for optional IIIF validator JP2/PDF output tests.

Those tests are Level 3 in Image API 3.0 and are not selected by the Level 2
command in VALIDATOR.md. This stub allows the validator to import its complete
test package on hosts without system libmagic. It fails if a test uses it.
"""


class Magic:
    def __init__(self, *args, **kwargs):
        raise RuntimeError("libmagic is required for optional JP2/PDF format tests")
