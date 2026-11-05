use axum::{
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::{config::Config, server::AppState};

use super::{Step, base_uri, source};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InfoMedia {
    JsonLd,
    Json,
}

fn info_media(headers: &HeaderMap) -> Option<InfoMedia> {
    if !headers.contains_key(header::ACCEPT) {
        return Some(InfoMedia::JsonLd);
    }
    let mut json_ld: Option<(u8, f64)> = None;
    let mut json: Option<(u8, f64)> = None;
    for value in headers.get_all(header::ACCEPT) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        for item in value.split(',') {
            let mut parts = item.split(';');
            let media_type = parts.next().unwrap_or("").trim().to_ascii_lowercase();
            let mut quality = 1.0;
            let mut profile_valid = true;
            for parameter in parts {
                if let Some((name, value)) = parameter.trim().split_once('=') {
                    if name.trim().eq_ignore_ascii_case("q") {
                        quality = value.trim().parse::<f64>().unwrap_or(0.0);
                    } else if name.trim().eq_ignore_ascii_case("profile") {
                        profile_valid = value.trim().trim_matches('"')
                            == "http://iiif.io/api/image/3/context.json";
                    }
                }
            }
            if !quality.is_finite() || !(0.0..=1.0).contains(&quality) {
                continue;
            }
            let candidates: &[(InfoMedia, u8)] = match media_type.as_str() {
                "application/ld+json" if profile_valid => &[(InfoMedia::JsonLd, 2)],
                "application/json" => &[(InfoMedia::Json, 2)],
                "application/*" => &[(InfoMedia::JsonLd, 1), (InfoMedia::Json, 1)],
                "*/*" => &[(InfoMedia::JsonLd, 0), (InfoMedia::Json, 0)],
                _ => &[],
            };
            for &(media, specificity) in candidates {
                let current = match media {
                    InfoMedia::JsonLd => &mut json_ld,
                    InfoMedia::Json => &mut json,
                };
                if current
                    .as_ref()
                    .is_none_or(|(spec, q)| (specificity, quality) > (*spec, *q))
                {
                    *current = Some((specificity, quality));
                }
            }
        }
    }
    let ld_quality = json_ld.map(|(_, q)| q).unwrap_or(0.0);
    let json_quality = json.map(|(_, q)| q).unwrap_or(0.0);
    if ld_quality == 0.0 && json_quality == 0.0 {
        None
    } else if json_quality > ld_quality {
        Some(InfoMedia::Json)
    } else {
        Some(InfoMedia::JsonLd)
    }
}

fn info_sizes(width: u32, height: u32, min_size: u32, max_pixels: u64) -> Vec<serde_json::Value> {
    let full_area = u64::from(width) * u64::from(height);

    // Cantaloupe computes the lower bound by repeatedly quartering the full
    // image area. This keeps every advertised size on the source image's
    // power-of-two series instead of first scaling to an arbitrary max area.
    let mut min_reduction = 0u32;
    let mut area_divisor = 1u128;
    while u128::from(full_area) > u128::from(max_pixels) * area_divisor {
        min_reduction += 1;
        area_divisor *= 4;
    }

    // The full size is always eligible at this end of the range. Additional
    // halvings are eligible while the smaller unrounded dimension remains at
    // least min_size.
    let mut max_reduction = 0u32;
    let smaller_dimension = u64::from(width.min(height));
    while smaller_dimension >= u64::from(min_size) * (1u64 << (max_reduction + 1)) {
        max_reduction += 1;
    }

    if min_reduction > max_reduction {
        return Vec::new();
    }

    (min_reduction..=max_reduction)
        .rev()
        .map(|reduction| {
            let divisor = 1u64 << reduction;
            // Java's Math.round(), used by Cantaloupe, rounds positive halves
            // up. Do the same directly from the virtual full dimensions.
            let rounded_width = (u64::from(width) + divisor / 2) / divisor;
            let rounded_height = (u64::from(height) + divisor / 2) / divisor;
            serde_json::json!({"width": rounded_width, "height": rounded_height})
        })
        .collect()
}

fn info_document(config: &Config, identifier: &str, width: u32, height: u32) -> serde_json::Value {
    let sizes = info_sizes(width, height, config.min_size, config.max_output_pixels);
    let tile_width = config.min_tile_size.max(1);
    let mut factors = Vec::new();
    let mut factor = 1u32;
    loop {
        factors.push(factor);
        if width.max(height) / factor <= tile_width || factor > u32::MAX / 2 {
            break;
        }
        factor *= 2;
    }
    let mut info = serde_json::json!({
        "@context": "http://iiif.io/api/image/3/context.json",
        "id": base_uri(config, identifier),
        "type": "ImageService3",
        "protocol": "http://iiif.io/api/image",
        "profile": "level2",
        "width": width,
        "height": height,
        "maxArea": config.max_output_pixels,
        "sizes": sizes,
        "preferredFormats": ["webp", "avif", "jpg"],
        "extraFormats": ["webp", "avif", "jxl"],
        "extraQualities": ["color", "gray"]
    });
    if width.max(height) >= tile_width
        && u64::from(tile_width) * u64::from(tile_width) <= config.max_output_pixels
    {
        info["tiles"] = serde_json::json!([{"width": tile_width, "scaleFactors": factors}]);
    }
    info
}

pub(super) async fn serve(
    state: &AppState,
    identifier: &str,
    headers: &HeaderMap,
) -> Step<Response> {
    let media = info_media(headers).ok_or_else(|| {
        (
            StatusCode::NOT_ACCEPTABLE,
            [(header::VARY, "Accept")],
            "unsupported Accept header",
        )
            .into_response()
    })?;
    let generation = source::current_generation(state, identifier).await?;
    let source = source::fetch(state, identifier, &generation).await?;
    let ((width, height, _), _) = source::probe(&source.path).await?;
    let body = serde_json::to_vec(&info_document(&state.config, identifier, width, height))
        .expect("serialize JSON");
    let content_type = match media {
        InfoMedia::JsonLd => {
            "application/ld+json;profile=\"http://iiif.io/api/image/3/context.json\""
        }
        InfoMedia::Json => "application/json",
    };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::VARY, "Accept"),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::{InfoMedia, info_media, info_sizes};
    use axum::http::{HeaderMap, header};

    #[test]
    fn negotiates_image_information_media_type() {
        assert_eq!(info_media(&HeaderMap::new()), Some(InfoMedia::JsonLd));
        for (accept, expected) in [
            ("application/json", Some(InfoMedia::Json)),
            ("application/ld+json", Some(InfoMedia::JsonLd)),
            ("*/*", Some(InfoMedia::JsonLd)),
            (
                "application/json;q=0.9, application/ld+json;q=0.5",
                Some(InfoMedia::Json),
            ),
            (
                "application/json, application/ld+json;q=0",
                Some(InfoMedia::Json),
            ),
            ("text/html", None),
            ("application/json;q=0", None),
            ("application/ld+json;q=0, */*;q=1", Some(InfoMedia::Json)),
            (
                "application/ld+json;profile=\"https://example.org/other\"",
                None,
            ),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT, accept.parse().unwrap());
            assert_eq!(info_media(&headers), expected, "{accept}");
        }
    }

    #[test]
    fn information_sizes_match_cantaloupes_power_of_two_series() {
        assert_eq!(
            info_sizes(1500, 1200, 64, 100_000_000),
            vec![
                serde_json::json!({"width": 94, "height": 75}),
                serde_json::json!({"width": 188, "height": 150}),
                serde_json::json!({"width": 375, "height": 300}),
                serde_json::json!({"width": 750, "height": 600}),
                serde_json::json!({"width": 1500, "height": 1200}),
            ]
        );
    }

    #[test]
    fn information_sizes_apply_upper_and_lower_reduction_bounds() {
        assert_eq!(
            info_sizes(1500, 1200, 64, 112_500),
            vec![
                serde_json::json!({"width": 94, "height": 75}),
                serde_json::json!({"width": 188, "height": 150}),
                serde_json::json!({"width": 375, "height": 300}),
            ]
        );
        assert!(info_sizes(1500, 1200, 64, 1_000).is_empty());
    }

    #[test]
    fn information_sizes_round_each_size_from_the_full_dimensions() {
        assert_eq!(
            info_sizes(5, 5, 1, 100),
            vec![
                serde_json::json!({"width": 1, "height": 1}),
                serde_json::json!({"width": 3, "height": 3}),
                serde_json::json!({"width": 5, "height": 5}),
            ]
        );
        assert_eq!(
            info_sizes(32, 16, 64, 1_000),
            vec![serde_json::json!({"width": 32, "height": 16})]
        );
    }
}
