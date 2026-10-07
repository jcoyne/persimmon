use persimmon::iiif::{self, Format, Quality, Rect, Route};

fn geometry(path: &str) -> (Rect, (u32, u32)) {
    let Route::Image(request) = iiif::parse_route(path).unwrap() else {
        panic!("expected image request");
    };
    let region = iiif::region_rect(&request.region, 600, 400).unwrap();
    let size = iiif::output_size(&request.size, region, 10_000_000).unwrap();
    (region, size)
}

#[test]
fn level2_region_forms() {
    let cases = [
        (
            "full",
            Rect {
                x: 0,
                y: 0,
                width: 600,
                height: 400,
            },
        ),
        (
            "square",
            Rect {
                x: 100,
                y: 0,
                width: 400,
                height: 400,
            },
        ),
        (
            "100,50,200,100",
            Rect {
                x: 100,
                y: 50,
                width: 200,
                height: 100,
            },
        ),
        (
            "pct:25,25,50,50",
            Rect {
                x: 150,
                y: 100,
                width: 300,
                height: 200,
            },
        ),
        (
            "550,350,100,100",
            Rect {
                x: 550,
                y: 350,
                width: 50,
                height: 50,
            },
        ),
    ];
    for (form, expected) in cases {
        let path = format!("/id/{form}/max/0/default.jpg");
        assert_eq!(geometry(&path).0, expected, "{form}");
    }
}

#[test]
fn level2_size_forms() {
    let cases = [
        ("max", (600, 400)),
        ("300,", (300, 200)),
        (",200", (300, 200)),
        ("300,200", (300, 200)),
        ("pct:50", (300, 200)),
        ("!300,300", (300, 200)),
    ];
    for (form, expected) in cases {
        let path = format!("/id/full/{form}/0/default.jpg");
        assert_eq!(geometry(&path).1, expected, "{form}");
    }
    let one_pixel = Rect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    assert!(iiif::output_size(&iiif::Size::Percent(1.0), one_pixel, 1).is_err());
}

#[test]
fn confined_bounds_larger_than_source_do_not_upscale() {
    let source = Rect {
        x: 0,
        y: 0,
        width: 1000,
        height: 1000,
    };
    assert_eq!(
        iiif::output_size(&iiif::Size::Confined(2000, 3000), source, 10_000_000).unwrap(),
        (1000, 1000)
    );
    assert!(iiif::output_size(&iiif::Size::Exact(2000, 3000), source, 10_000_000).is_err());
}

#[test]
fn required_rotations_qualities_and_formats_parse() {
    for rotation in ["0", "90", "180", "270"] {
        for (quality, expected_quality) in [
            ("default", Quality::Default),
            ("color", Quality::Color),
            ("gray", Quality::Gray),
        ] {
            for (format, expected_format) in [
                ("jpg", Format::Jpeg),
                ("png", Format::Png),
                ("webp", Format::Webp),
                ("avif", Format::Avif),
            ] {
                let path = format!("/id/full/max/{rotation}/{quality}.{format}");
                let Route::Image(request) = iiif::parse_route(&path).unwrap() else {
                    panic!("image");
                };
                assert_eq!(request.rotation.to_string(), rotation);
                assert_eq!(request.quality, expected_quality);
                assert_eq!(request.format, expected_format);
            }
        }
    }
}

#[test]
fn invalid_and_optional_operations_do_not_succeed() {
    for path in [
        "/id/full/^600,/0/default.jpg",
        "/id/full/max/!0/default.jpg",
        "/id/full/max/22.5/default.jpg",
        "/id/full/max/0/bitonal.jpg",
        "/id/full/max/0/default.tif",
        "/id/full/max/0/default.gif",
    ] {
        assert!(iiif::parse_route(path).is_err(), "{path}");
    }
    let tiny = iiif::parse_route("/id/pct:99.9,99.9,0.1,0.1/max/0/default.jpg").unwrap();
    let Route::Image(request) = tiny else {
        panic!("image");
    };
    assert!(iiif::region_rect(&request.region, 600, 400).is_err());
    assert_eq!(
        iiif::parse_route("/id/full/^pct:120/0/default.jpg")
            .unwrap_err()
            .0,
        "upscaling is not supported"
    );
    assert_eq!(
        iiif::parse_route("/id/full/pct:200/0/default.jpg")
            .unwrap_err()
            .0,
        "invalid output size or upscaling"
    );
    assert!(iiif::parse_route("/id/full/^bad/0/default.jpg").is_err());
}
