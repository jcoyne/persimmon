#include <algorithm>
#include <climits>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <vector>

#include "kdu_compressed.h"
#include "jp2.h"
#include "kdu_region_decompressor.h"

using namespace kdu_supp;

extern "C" const char *persimmon_kakadu_version() {
  return kdu_get_core_version();
}

extern "C" void persimmon_kakadu_free(void *buffer) { std::free(buffer); }

// Decodes a region expressed in full-resolution image coordinates. The output
// buffer is owned by the caller and must be released with persimmon_kakadu_free.
// Each call owns its source, codestream, and decompressor for thread safety.
// Return 6 when the reduced region exceeds max_pixels, or 7 when this source
// needs the command-line fallback for its component geometry.
extern "C" int persimmon_kakadu_decode_reduced_region(
    const char *path, uint32_t x, uint32_t y, uint32_t width, uint32_t height,
    uint32_t desired_discard, uint64_t max_pixels, uint8_t **rgb,
    uint32_t *output_width, uint32_t *output_height) {
  if (path == nullptr || rgb == nullptr || output_width == nullptr ||
      output_height == nullptr || width == 0 || height == 0 ||
      width > INT_MAX || height > INT_MAX || x > INT_MAX || y > INT_MAX ||
      desired_discard > 30 || max_pixels == 0)
    return 1;
  *rgb = nullptr;
  *output_width = *output_height = 0;

  jp2_family_src family;
  jp2_source source;
  kdu_codestream codestream;
  try {
    family.open(path);
    if (!source.open(&family) || !source.read_header())
      return 2;
    codestream.create(&source);
    kdu_channel_mapping channels;
    channels.configure(&source, false);
    const int first_component = channels.get_source_component(0);
    if (first_component < 0) {
      codestream.destroy();
      return 7;
    }
    kdu_coords subsampling;
    codestream.get_subsampling(first_component, subsampling, true);
    if (subsampling.x != 1 || subsampling.y != 1) {
      codestream.destroy();
      return 7;
    }

    kdu_region_decompressor decompressor;
    decompressor.set_white_stretch(8);
    const kdu_coords unit(1, 1);
    const kdu_dims full = decompressor.get_rendered_image_dims(
        codestream, &channels, -1, 0, unit, unit);
    if (full.size.x <= 0 || full.size.y <= 0 ||
        uint64_t(x) + width > uint32_t(full.size.x) ||
        uint64_t(y) + height > uint32_t(full.size.y) ||
        int64_t(full.pos.x) + x > INT_MAX ||
        int64_t(full.pos.y) + y > INT_MAX) {
      codestream.destroy();
      return 3;
    }
    kdu_dims original_region;
    original_region.pos = kdu_coords(full.pos.x + int(x), full.pos.y + int(y));
    original_region.size = kdu_coords(int(width), int(height));

    const int discard =
        std::min<int>(desired_discard, codestream.get_min_dwt_levels());
    const int divisor = 1 << discard;
    const kdu_dims reduced_full = decompressor.get_rendered_image_dims(
        codestream, &channels, -1, discard, unit, unit);
    kdu_dims region = kdu_region_decompressor::find_render_dims(
        original_region, kdu_coords(divisor, divisor), unit, unit);
    const int64_t left = std::max<int64_t>(region.pos.x, reduced_full.pos.x);
    const int64_t top = std::max<int64_t>(region.pos.y, reduced_full.pos.y);
    const int64_t right = std::min<int64_t>(
        int64_t(region.pos.x) + region.size.x,
        int64_t(reduced_full.pos.x) + reduced_full.size.x);
    const int64_t bottom = std::min<int64_t>(
        int64_t(region.pos.y) + region.size.y,
        int64_t(reduced_full.pos.y) + reduced_full.size.y);
    if (left >= right || top >= bottom || right > INT_MAX || bottom > INT_MAX) {
      codestream.destroy();
      return 3;
    }
    region.pos = kdu_coords(int(left), int(top));
    region.size = kdu_coords(int(right - left), int(bottom - top));
    const uint64_t pixels = uint64_t(region.size.x) * uint64_t(region.size.y);
    if (pixels > max_pixels || pixels > SIZE_MAX / 3) {
      codestream.destroy();
      return 6;
    }

    std::vector<kdu_int32> packed(static_cast<size_t>(pixels), 0);
    decompressor.start(codestream, &channels, -1, discard, INT_MAX, region,
                       unit, unit);
    kdu_dims incomplete = region;
    kdu_dims produced;
    while (decompressor.process(packed.data(), region.pos, region.size.x,
                                65536, 0, incomplete, produced)) {}
    kdu_exception exception = KDU_NULL_EXCEPTION;
    const bool finished = decompressor.finish(&exception);
    if (!finished || !incomplete.is_empty()) {
      codestream.destroy();
      return 4;
    }
    codestream.destroy();
    uint8_t *buffer = static_cast<uint8_t *>(std::malloc(size_t(pixels * 3)));
    if (buffer == nullptr)
      return 8;
    for (size_t i = 0; i < size_t(pixels); ++i) {
      const uint32_t pixel = uint32_t(packed[i]);
      buffer[i * 3] = uint8_t(pixel >> 16);
      buffer[i * 3 + 1] = uint8_t(pixel >> 8);
      buffer[i * 3 + 2] = uint8_t(pixel);
    }
    *rgb = buffer;
    *output_width = uint32_t(region.size.x);
    *output_height = uint32_t(region.size.y);
    return 0;
  } catch (...) {
    if (codestream.exists())
      codestream.destroy();
    return 5;
  }
}

// Kept for compatibility with callers of the initial full-resolution adapter.
extern "C" int persimmon_kakadu_decode_region(
    const char *path, uint32_t x, uint32_t y, uint32_t width, uint32_t height,
    uint8_t *rgb, size_t rgb_length) {
  if (rgb == nullptr || uint64_t(width) * height > SIZE_MAX / 3 ||
      rgb_length < uint64_t(width) * height * 3)
    return 1;
  uint8_t *decoded = nullptr;
  uint32_t decoded_width = 0, decoded_height = 0;
  const int status = persimmon_kakadu_decode_reduced_region(
      path, x, y, width, height, 0, uint64_t(width) * height, &decoded,
      &decoded_width, &decoded_height);
  if (status != 0)
    return status;
  if (decoded_width != width || decoded_height != height) {
    persimmon_kakadu_free(decoded);
    return 4;
  }
  std::memcpy(rgb, decoded, size_t(uint64_t(width) * height * 3));
  persimmon_kakadu_free(decoded);
  return 0;
}
