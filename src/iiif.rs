use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Region {
    Full,
    Square,
    Pixels(u32, u32, u32, u32),
    Percent(f64, f64, f64, f64),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Size {
    Max,
    Width(u32),
    Height(u32),
    Exact(u32, u32),
    Percent(f64),
    Confined(u32, u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Default,
    Color,
    Gray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
    Avif,
}

impl Format {
    pub fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageRequest {
    pub identifier: String,
    pub region: Region,
    pub size: Size,
    pub rotation: u16,
    pub quality: Quality,
    pub format: Format,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Base(String),
    Info(String),
    Image(ImageRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

fn positive_u32(s: &str) -> Result<u32, Error> {
    let n = s
        .parse::<u32>()
        .map_err(|_| Error("invalid positive integer"))?;
    if n == 0 {
        Err(Error("zero dimension"))
    } else {
        Ok(n)
    }
}

fn nonnegative_u32(s: &str) -> Result<u32, Error> {
    s.parse::<u32>()
        .map_err(|_| Error("invalid nonnegative integer"))
}

fn positive_decimal(s: &str) -> Result<f64, Error> {
    if s.is_empty()
        || !s.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || s.bytes().filter(|b| *b == b'.').count() > 1
    {
        return Err(Error("invalid decimal"));
    }
    let n = s.parse::<f64>().map_err(|_| Error("invalid decimal"))?;
    if n.is_finite() && n > 0.0 {
        Ok(n)
    } else {
        Err(Error("nonpositive decimal"))
    }
}

fn nonnegative_decimal(s: &str) -> Result<f64, Error> {
    if s.is_empty()
        || !s.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || s.bytes().filter(|b| *b == b'.').count() > 1
    {
        return Err(Error("invalid decimal"));
    }
    let n = s.parse::<f64>().map_err(|_| Error("invalid decimal"))?;
    if n.is_finite() && n >= 0.0 {
        Ok(n)
    } else {
        Err(Error("invalid decimal"))
    }
}

fn parse_region(s: &str) -> Result<Region, Error> {
    if s == "full" {
        return Ok(Region::Full);
    }
    if s == "square" {
        return Ok(Region::Square);
    }
    let (percent, values) = if let Some(v) = s.strip_prefix("pct:") {
        (true, v)
    } else {
        (false, s)
    };
    let parts: Vec<_> = values.split(',').collect();
    if parts.len() != 4 {
        return Err(Error("invalid region"));
    }
    if percent {
        Ok(Region::Percent(
            nonnegative_decimal(parts[0])?,
            nonnegative_decimal(parts[1])?,
            positive_decimal(parts[2])?,
            positive_decimal(parts[3])?,
        ))
    } else {
        Ok(Region::Pixels(
            nonnegative_u32(parts[0])?,
            nonnegative_u32(parts[1])?,
            positive_u32(parts[2])?,
            positive_u32(parts[3])?,
        ))
    }
}

fn parse_size(s: &str) -> Result<Size, Error> {
    if let Some(upscaled) = s.strip_prefix('^') {
        if let Some(percent) = upscaled.strip_prefix("pct:") {
            positive_decimal(percent)?;
        } else {
            parse_size(upscaled)?;
        }
        return Err(Error("upscaling is not supported"));
    }
    if s == "max" {
        return Ok(Size::Max);
    }
    if let Some(n) = s.strip_prefix("pct:") {
        let n = positive_decimal(n)?;
        return if n <= 100.0 {
            Ok(Size::Percent(n))
        } else {
            Err(Error("invalid output size or upscaling"))
        };
    }
    if let Some(dims) = s.strip_prefix('!') {
        let (w, h) = dims.split_once(',').ok_or(Error("invalid confined size"))?;
        return Ok(Size::Confined(positive_u32(w)?, positive_u32(h)?));
    }
    let (w, h) = s.split_once(',').ok_or(Error("invalid size"))?;
    match (w.is_empty(), h.is_empty()) {
        (false, true) => Ok(Size::Width(positive_u32(w)?)),
        (true, false) => Ok(Size::Height(positive_u32(h)?)),
        (false, false) => Ok(Size::Exact(positive_u32(w)?, positive_u32(h)?)),
        _ => Err(Error("invalid size")),
    }
}

fn parse_rotation(s: &str) -> Result<u16, Error> {
    if s.starts_with('!') {
        return Err(Error("mirroring is not supported"));
    }
    match s {
        "0" => Ok(0),
        "90" => Ok(90),
        "180" => Ok(180),
        "270" => Ok(270),
        _ => Err(Error("rotation is not supported")),
    }
}

fn parse_quality_format(s: &str) -> Result<(Quality, Format), Error> {
    let (quality, format) = s
        .rsplit_once('.')
        .ok_or(Error("invalid quality and format"))?;
    let quality = match quality {
        "default" => Quality::Default,
        "color" => Quality::Color,
        "gray" => Quality::Gray,
        _ => return Err(Error("quality is not supported")),
    };
    let format = match format {
        "jpg" => Format::Jpeg,
        "png" => Format::Png,
        "webp" => Format::Webp,
        "avif" => Format::Avif,
        _ => return Err(Error("format is not supported")),
    };
    Ok((quality, format))
}

fn decode_identifier(raw: &str) -> Result<String, Error> {
    let mut bytes = Vec::with_capacity(raw.len());
    let source = raw.as_bytes();
    let mut i = 0;
    while i < source.len() {
        if source[i] == b'%' {
            if i + 2 >= source.len() {
                return Err(Error("invalid percent encoding"));
            }
            let hex = std::str::from_utf8(&source[i + 1..i + 3])
                .map_err(|_| Error("invalid percent encoding"))?;
            bytes.push(u8::from_str_radix(hex, 16).map_err(|_| Error("invalid percent encoding"))?);
            i += 3;
        } else {
            bytes.push(source[i]);
            i += 1;
        }
    }
    let identifier = String::from_utf8(bytes).map_err(|_| Error("identifier is not UTF-8"))?;
    if !valid_identifier(&identifier) {
        return Err(Error("invalid identifier"));
    }
    Ok(identifier)
}

/// The decoded identifier is used unchanged as an S3 key. Reject empty and
/// dot segments so a path-normalizing S3 endpoint or proxy cannot resolve a
/// key outside the source bucket.
pub fn valid_identifier(identifier: &str) -> bool {
    !identifier.contains('\0')
        && identifier
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// Parse a raw HTTP path after removal of an optional service prefix. The
/// identifier is one raw path segment; slashes inside it must be encoded.
pub fn parse_route(raw_path: &str) -> Result<Route, Error> {
    let path = raw_path.strip_prefix('/').ok_or(Error("invalid path"))?;
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        [id] if !id.is_empty() => Ok(Route::Base(decode_identifier(id)?)),
        [id, "info.json"] => Ok(Route::Info(decode_identifier(id)?)),
        [id, region, size, rotation, tail] => {
            let (quality, format) = parse_quality_format(tail)?;
            Ok(Route::Image(ImageRequest {
                identifier: decode_identifier(id)?,
                region: parse_region(region)?,
                size: parse_size(size)?,
                rotation: parse_rotation(rotation)?,
                quality,
                format,
            }))
        }
        _ => Err(Error("invalid IIIF route")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub fn region_rect(region: &Region, width: u32, height: u32) -> Result<Rect, Error> {
    if width == 0 || height == 0 {
        return Err(Error("invalid source dimensions"));
    }
    let raw = match region {
        Region::Full => Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
        Region::Square => {
            let side = width.min(height);
            Rect {
                x: (width - side) / 2,
                y: (height - side) / 2,
                width: side,
                height: side,
            }
        }
        Region::Pixels(x, y, w, h) => Rect {
            x: *x,
            y: *y,
            width: *w,
            height: *h,
        },
        Region::Percent(x, y, w, h) => Rect {
            x: (width as f64 * x / 100.0).floor() as u32,
            y: (height as f64 * y / 100.0).floor() as u32,
            width: (width as f64 * w / 100.0).round() as u32,
            height: (height as f64 * h / 100.0).round() as u32,
        },
    };
    if raw.x >= width || raw.y >= height || raw.width == 0 || raw.height == 0 {
        return Err(Error("region outside image"));
    }
    Ok(Rect {
        width: raw.width.min(width - raw.x),
        height: raw.height.min(height - raw.y),
        ..raw
    })
}

pub fn output_size(size: &Size, region: Rect, max_pixels: u64) -> Result<(u32, u32), Error> {
    let (rw, rh) = (region.width as f64, region.height as f64);
    let (w, h) = match size {
        Size::Max => max_size(region.width, region.height, max_pixels),
        Size::Width(w) => (*w, (rh * *w as f64 / rw).round().max(1.0) as u32),
        Size::Height(h) => ((rw * *h as f64 / rh).round().max(1.0) as u32, *h),
        Size::Exact(w, h) => (*w, *h),
        Size::Percent(p) => (
            (rw * p / 100.0).round() as u32,
            (rh * p / 100.0).round() as u32,
        ),
        Size::Confined(w, h) => {
            let scale = (1.0_f64).min(*w as f64 / rw).min(*h as f64 / rh);
            (
                (rw * scale).round().max(1.0) as u32,
                (rh * scale).round().max(1.0) as u32,
            )
        }
    };
    if w == 0 || h == 0 || w > region.width || h > region.height {
        return Err(Error("invalid output size or upscaling"));
    }
    if u64::from(w) * u64::from(h) > max_pixels {
        return Err(Error("output exceeds pixel limit"));
    }
    Ok((w, h))
}

pub fn max_size(width: u32, height: u32, max_pixels: u64) -> (u32, u32) {
    if u64::from(width) * u64::from(height) <= max_pixels {
        return (width, height);
    }
    let scale = (max_pixels as f64 / (width as f64 * height as f64)).sqrt();
    let mut w = (width as f64 * scale).floor().max(1.0) as u32;
    let mut h = (height as f64 * scale).floor().max(1.0) as u32;
    while u64::from(w) * u64::from(h) > max_pixels {
        if w >= h {
            w -= 1;
        } else {
            h -= 1;
        }
    }
    (w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_slashes_are_identifier_content() {
        let route = parse_route("/a%2Fb/full/!256,256/90/default.webp").unwrap();
        let Route::Image(req) = route else {
            panic!("expected image");
        };
        assert_eq!(req.identifier, "a/b");
        assert_eq!(req.size, Size::Confined(256, 256));
    }

    #[test]
    fn rejects_identifiers_with_empty_or_dot_segments() {
        for id in [
            "..",
            ".",
            "%2E%2E",
            "..%2Fother-bucket%2Fsecret",
            "a%2F..%2F..%2Fsecret",
            "a%2F.%2Fb",
            "a%2F..",
            "%2Fa",
            "a%2F",
            "a%2F%2Fb",
            "a%00b",
        ] {
            assert!(parse_route(&format!("/{id}/info.json")).is_err(), "{id}");
        }
        for id in ["a..b", "..a", "a.", ".hidden", "a%2F..b%2Fc.jp2"] {
            assert!(parse_route(&format!("/{id}/info.json")).is_ok(), "{id}");
        }
    }

    #[test]
    fn region_clips_at_edge() {
        assert_eq!(
            region_rect(&Region::Pixels(90, 80, 30, 40), 100, 100).unwrap(),
            Rect {
                x: 90,
                y: 80,
                width: 10,
                height: 20
            }
        );
    }

    #[test]
    fn refuses_unadvertised_upscaling() {
        let r = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 50,
        };
        assert!(output_size(&Size::Width(101), r, 10000).is_err());
        assert_eq!(
            output_size(&Size::Confined(200, 20), r, 10000).unwrap(),
            (40, 20)
        );
    }

    #[test]
    fn max_respects_area() {
        assert_eq!(max_size(1000, 1000, 250_000), (500, 500));
    }
}
