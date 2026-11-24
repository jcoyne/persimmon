//! Finds where each resolution's packets end in a JP2 source, so a decode that
//! discards resolution levels can start before the rest of the file arrives.
//!
//! Only one layout is supported: one tile in one tile-part, in a progression
//! that sends every packet of a resolution before any of the next (RPCL, RLCP,
//! or LRCP with one layer), with PLT markers giving each packet's length. The
//! packet count the header implies must match the PLT markers, and their
//! lengths must add up to the tile-part, or the source is reported unsupported.
//! A wrong answer here would decode wrong pixels without any error, so anything
//! unusual falls back to waiting for the whole file.

/// What the leading bytes of a source say about its layout.
#[derive(Debug, PartialEq, Eq)]
pub enum Layout {
    /// The header continues past the bytes given.
    NeedMore,
    /// Decodes must wait for the whole file.
    Unsupported(&'static str),
    Resolutions(ResolutionEnds),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ResolutionEnds {
    /// File offset just past the last packet of each resolution, lowest first.
    ends: Vec<u64>,
    /// Offset of the first byte after the packet data: the EOC marker, then any
    /// boxes that follow the codestream. j2k reads these, so they must be on
    /// disk before a partial decode.
    pub tail_start: u64,
}

impl ResolutionEnds {
    /// How many bytes from the start of the file a decode discarding `level`
    /// resolution levels reads, besides the tail. Levels beyond the codestream's
    /// are clamped, as the decoder does.
    pub fn prefix_for(&self, level: u32) -> u64 {
        let top = self.ends.len() - 1;
        let resolution = top - (level as usize).min(top);
        self.ends[resolution]
    }
}

const SOC: u16 = 0xff4f;
const SIZ: u16 = 0xff51;
const COD: u16 = 0xff52;
const COC: u16 = 0xff53;
const PLM: u16 = 0xff57;
const PLT: u16 = 0xff58;
const POC: u16 = 0xff5f;
const PPM: u16 = 0xff60;
const PPT: u16 = 0xff61;
const SOT: u16 = 0xff90;
const SOD: u16 = 0xff93;

const RLCP: u8 = 1;
const RPCL: u8 = 2;
const LRCP: u8 = 0;

/// Bytes read past the end of what was given.
struct Short;

type Parse<T> = Result<T, Short>;

fn u8_at(data: &[u8], pos: u64) -> Parse<u8> {
    usize::try_from(pos)
        .ok()
        .and_then(|pos| data.get(pos).copied())
        .ok_or(Short)
}

fn u16_at(data: &[u8], pos: u64) -> Parse<u16> {
    Ok(u16::from_be_bytes([
        u8_at(data, pos)?,
        u8_at(data, pos + 1)?,
    ]))
}

fn u32_at(data: &[u8], pos: u64) -> Parse<u32> {
    Ok(u32::from(u16_at(data, pos)?) << 16 | u32::from(u16_at(data, pos + 2)?))
}

fn u64_at(data: &[u8], pos: u64) -> Parse<u64> {
    Ok(u64::from(u32_at(data, pos)?) << 32 | u64::from(u32_at(data, pos + 4)?))
}

fn slice(data: &[u8], start: u64, end: u64) -> Parse<&[u8]> {
    let start = usize::try_from(start).map_err(|_| Short)?;
    let end = usize::try_from(end).map_err(|_| Short)?;
    data.get(start..end).ok_or(Short)
}

/// Reads the layout from `prefix`, the first bytes of a file `file_len` long.
pub fn locate(prefix: &[u8], file_len: u64) -> Layout {
    match parse(prefix, file_len) {
        Ok(layout) => layout,
        Err(Short) => Layout::NeedMore,
    }
}

fn parse(data: &[u8], file_len: u64) -> Parse<Layout> {
    let Some((start, end)) = find_codestream(data, file_len)? else {
        return Ok(Layout::Unsupported("not a JP2 with a codestream box"));
    };
    let mut pos = start;
    if u16_at(data, pos)? != SOC {
        return Ok(Layout::Unsupported("codestream does not start with SOC"));
    }
    pos += 2;
    let mut siz = None;
    let mut cod = None;
    // Main header.
    loop {
        let marker = u16_at(data, pos)?;
        if marker == SOT {
            break;
        }
        let len = u64::from(u16_at(data, pos + 2)?);
        if len < 2 {
            return Ok(Layout::Unsupported("invalid marker segment length"));
        }
        let segment = slice(data, pos + 4, pos + 2 + len)?;
        match marker {
            SIZ => match Siz::parse(segment) {
                Some(parsed) => siz = Some(parsed),
                None => return Ok(Layout::Unsupported("invalid SIZ")),
            },
            COD => match Cod::parse(segment) {
                Some(parsed) => cod = Some(parsed),
                None => return Ok(Layout::Unsupported("invalid COD")),
            },
            COC => return Ok(Layout::Unsupported("component coding style (COC)")),
            POC => return Ok(Layout::Unsupported("progression order change (POC)")),
            PPM => return Ok(Layout::Unsupported("packed packet headers (PPM)")),
            PLM => {
                return Ok(Layout::Unsupported(
                    "packet lengths in the main header (PLM)",
                ));
            }
            _ if marker >> 8 != 0xff => return Ok(Layout::Unsupported("invalid marker")),
            _ => {}
        }
        pos += 2 + len;
    }
    let (Some(siz), Some(cod)) = (siz, cod) else {
        return Ok(Layout::Unsupported("missing SIZ or COD"));
    };
    if siz.tiles() != 1 {
        return Ok(Layout::Unsupported("more than one tile"));
    }
    let resolution_major = match cod.progression {
        RPCL | RLCP => true,
        LRCP => cod.layers == 1,
        _ => false,
    };
    if !resolution_major {
        return Ok(Layout::Unsupported("progression is not resolution-major"));
    }

    // The tile-part header.
    let sot = pos;
    if u16_at(data, sot + 2)? != 10 {
        return Ok(Layout::Unsupported("invalid SOT"));
    }
    let tile = u16_at(data, sot + 4)?;
    let psot = u64::from(u32_at(data, sot + 6)?);
    let part = u8_at(data, sot + 10)?;
    let parts = u8_at(data, sot + 11)?;
    if tile != 0 || part != 0 || parts > 1 {
        return Ok(Layout::Unsupported("more than one tile-part"));
    }
    // The codestream ends with EOC; the caller checks it is there once the tail
    // arrives, which also rules out a second tile-part.
    let tile_end = if psot == 0 { end - 2 } else { sot + psot };
    if tile_end + 2 != end {
        return Ok(Layout::Unsupported("tile-part does not end the codestream"));
    }
    // Find the end of the header before decoding packet lengths: the source
    // arrives in many chunks, and the PLT markers of a large image can run to
    // megabytes, so each incomplete attempt must stay cheap.
    pos = sot + 12;
    let mut plt = Vec::new();
    loop {
        let marker = u16_at(data, pos)?;
        if marker == SOD {
            pos += 2;
            break;
        }
        let len = u64::from(u16_at(data, pos + 2)?);
        if len < 2 {
            return Ok(Layout::Unsupported("invalid marker segment length"));
        }
        match marker {
            PLT if len < 3 => return Ok(Layout::Unsupported("invalid PLT")),
            PLT => {
                // j2k orders PLT segments by their Zplt index, not by position.
                // Insist the two agree, which also rules out an index that wraps.
                let index = u8_at(data, pos + 4)?;
                if plt
                    .last()
                    .is_some_and(|&(previous, _, _)| index <= previous)
                {
                    return Ok(Layout::Unsupported("PLT markers out of Zplt order"));
                }
                plt.push((index, pos + 5, pos + 2 + len));
            }
            COD | COC => return Ok(Layout::Unsupported("coding style in tile-part header")),
            POC => return Ok(Layout::Unsupported("progression order change (POC)")),
            PPT => return Ok(Layout::Unsupported("packed packet headers (PPT)")),
            _ if marker >> 8 != 0xff => return Ok(Layout::Unsupported("invalid marker")),
            _ => {}
        }
        pos += 2 + len;
    }
    let mut lengths = Vec::new();
    for (_, start, end) in plt {
        // Lengths in 7-bit groups, high bit set on all but the last. Like j2k,
        // decode each segment on its own: a length may not continue into the next.
        let mut pending = 0u64;
        let mut pending_bytes = 0u32;
        for &byte in slice(data, start, end)? {
            pending = pending << 7 | u64::from(byte & 0x7f);
            pending_bytes += 1;
            if pending_bytes > 5 || pending > u64::from(u32::MAX) {
                return Ok(Layout::Unsupported("invalid PLT"));
            }
            if byte & 0x80 == 0 {
                lengths.push(pending);
                pending = 0;
                pending_bytes = 0;
            }
        }
        if pending_bytes != 0 {
            return Ok(Layout::Unsupported("invalid PLT"));
        }
    }
    if lengths.is_empty() {
        return Ok(Layout::Unsupported("no PLT markers"));
    }
    let data_start = pos;
    let Some(packets) = siz.packets_per_resolution(&cod) else {
        return Ok(Layout::Unsupported("invalid image geometry"));
    };
    if packets.iter().sum::<u64>() != lengths.len() as u64 {
        return Ok(Layout::Unsupported(
            "PLT packet count does not match the header",
        ));
    }
    let mut ends = Vec::with_capacity(packets.len());
    let mut offset = data_start;
    let mut lengths = lengths.into_iter();
    for count in packets {
        for length in lengths.by_ref().take(count as usize) {
            offset += length;
        }
        ends.push(offset);
    }
    if offset != tile_end {
        return Ok(Layout::Unsupported(
            "PLT lengths do not add up to the tile-part",
        ));
    }
    Ok(Layout::Resolutions(ResolutionEnds {
        ends,
        tail_start: tile_end,
    }))
}

/// Finds the contents of the top-level contiguous codestream box.
fn find_codestream(data: &[u8], file_len: u64) -> Parse<Option<(u64, u64)>> {
    // The JP2 signature box comes first.
    if slice(data, 0, 12)? != b"\0\0\0\x0cjP  \r\n\x87\n" {
        return Ok(None);
    }
    let mut pos = 0u64;
    while pos + 8 <= file_len {
        let len32 = u32_at(data, pos)?;
        let kind = slice(data, pos + 4, pos + 8)?;
        let (len, header) = match len32 {
            0 => (file_len - pos, 8),
            1 => (u64_at(data, pos + 8)?, 16),
            n => (u64::from(n), 8),
        };
        if len < header || pos.checked_add(len).is_none_or(|end| end > file_len) {
            return Ok(None);
        }
        if kind == b"jp2c" {
            return Ok(Some((pos + header, pos + len)));
        }
        pos += len;
    }
    Ok(None)
}

struct Siz {
    width: u64,
    height: u64,
    x_offset: u64,
    y_offset: u64,
    tile_width: u64,
    tile_height: u64,
    tile_x_offset: u64,
    tile_y_offset: u64,
    /// Horizontal and vertical subsampling of each component.
    subsampling: Vec<(u64, u64)>,
}

impl Siz {
    fn parse(segment: &[u8]) -> Option<Self> {
        let at = |i: u64| u32_at(segment, i).ok().map(u64::from);
        let components = u16_at(segment, 34).ok()?;
        let mut subsampling = Vec::with_capacity(usize::from(components));
        for c in 0..u64::from(components) {
            let dx = u64::from(u8_at(segment, 37 + 3 * c).ok()?);
            let dy = u64::from(u8_at(segment, 38 + 3 * c).ok()?);
            if dx == 0 || dy == 0 {
                return None;
            }
            subsampling.push((dx, dy));
        }
        let siz = Self {
            width: at(2)?,
            height: at(6)?,
            x_offset: at(10)?,
            y_offset: at(14)?,
            tile_width: at(18)?,
            tile_height: at(22)?,
            tile_x_offset: at(26)?,
            tile_y_offset: at(30)?,
            subsampling,
        };
        let valid = siz.tile_width > 0
            && siz.tile_height > 0
            && siz.x_offset < siz.width
            && siz.y_offset < siz.height
            && siz.tile_x_offset <= siz.x_offset
            && siz.tile_y_offset <= siz.y_offset
            && !siz.subsampling.is_empty();
        valid.then_some(siz)
    }

    fn tiles(&self) -> u64 {
        (self.width - self.tile_x_offset).div_ceil(self.tile_width)
            * (self.height - self.tile_y_offset).div_ceil(self.tile_height)
    }

    /// Packets in each resolution of the one tile, lowest first: one per
    /// precinct, component and layer (ISO/IEC 15444-1 B.5, B.6).
    fn packets_per_resolution(&self, cod: &Cod) -> Option<Vec<u64>> {
        let levels = u32::from(cod.levels);
        let x0 = self.x_offset.max(self.tile_x_offset);
        let y0 = self.y_offset.max(self.tile_y_offset);
        let x1 = self.width.min(self.tile_x_offset + self.tile_width);
        let y1 = self.height.min(self.tile_y_offset + self.tile_height);
        let mut packets = vec![0u64; usize::from(cod.levels) + 1];
        for &(dx, dy) in &self.subsampling {
            let (cx0, cx1) = (x0.div_ceil(dx), x1.div_ceil(dx));
            let (cy0, cy1) = (y0.div_ceil(dy), y1.div_ceil(dy));
            for (r, count) in packets.iter_mut().enumerate() {
                let scale = 1u64.checked_shl(levels - r as u32)?;
                let (px, py) = cod.precinct(r);
                let precincts = |c0: u64, c1: u64, exponent: u8| {
                    let (r0, r1) = (c0.div_ceil(scale), c1.div_ceil(scale));
                    if r1 > r0 {
                        r1.div_ceil(1 << exponent) - (r0 >> exponent)
                    } else {
                        0
                    }
                };
                *count += precincts(cx0, cx1, px) * precincts(cy0, cy1, py) * u64::from(cod.layers);
            }
        }
        Some(packets)
    }
}

struct Cod {
    progression: u8,
    layers: u16,
    levels: u8,
    /// Precinct width and height exponents for each resolution, lowest first.
    /// Empty when the codestream uses the default, maximal precincts.
    precincts: Vec<(u8, u8)>,
}

impl Cod {
    fn parse(segment: &[u8]) -> Option<Self> {
        let style = u8_at(segment, 0).ok()?;
        let levels = u8_at(segment, 5).ok()?;
        if levels > 32 {
            return None;
        }
        let precincts = if style & 1 != 0 {
            (0..=u64::from(levels))
                .map(|r| u8_at(segment, 10 + r).ok().map(|b| (b & 0x0f, b >> 4)))
                .collect::<Option<Vec<_>>>()?
        } else {
            Vec::new()
        };
        Some(Self {
            progression: u8_at(segment, 1).ok()?,
            layers: u16_at(segment, 2).ok()?,
            levels,
            precincts,
        })
    }

    fn precinct(&self, resolution: usize) -> (u8, u8) {
        self.precincts.get(resolution).copied().unwrap_or((15, 15))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a JP2 with one tile-part whose packets have the given lengths.
    struct Builder {
        width: u32,
        height: u32,
        components: u8,
        progression: u8,
        layers: u16,
        levels: u8,
        precincts: Option<Vec<u8>>,
        tile_parts: u8,
        /// The Zplt index of each PLT segment, which share the packet lengths
        /// in order. Empty for no PLT markers.
        plt: Vec<u8>,
    }

    impl Default for Builder {
        fn default() -> Self {
            Self {
                width: 1000,
                height: 600,
                components: 3,
                progression: RPCL,
                layers: 2,
                levels: 2,
                precincts: None,
                tile_parts: 1,
                plt: vec![0],
            }
        }
    }

    fn segment(out: &mut Vec<u8>, marker: u16, body: &[u8]) {
        out.extend(marker.to_be_bytes());
        out.extend((body.len() as u16 + 2).to_be_bytes());
        out.extend(body);
    }

    impl Builder {
        fn build(&self, packet_lengths: &[u64]) -> Vec<u8> {
            let mut cs = SOC.to_be_bytes().to_vec();
            let mut siz = vec![0, 0];
            for v in [self.width, self.height, 0, 0, self.width, self.height, 0, 0] {
                siz.extend(v.to_be_bytes());
            }
            siz.extend(u16::from(self.components).to_be_bytes());
            for _ in 0..self.components {
                siz.extend([7, 1, 1]);
            }
            segment(&mut cs, SIZ, &siz);
            let mut cod = vec![u8::from(self.precincts.is_some()), self.progression];
            cod.extend(self.layers.to_be_bytes());
            cod.extend([0, self.levels, 4, 4, 0, 0]);
            cod.extend(self.precincts.iter().flatten());
            segment(&mut cs, COD, &cod);
            segment(&mut cs, 0xff5c, &[0x22, 0x88]);
            let mut header = Vec::new();
            let per_segment = packet_lengths.len().div_ceil(self.plt.len().max(1));
            for (&index, lengths) in self.plt.iter().zip(packet_lengths.chunks(per_segment)) {
                let mut plt = vec![index];
                for &len in lengths {
                    let mut groups = vec![(len & 0x7f) as u8];
                    let mut rest = len >> 7;
                    while rest > 0 {
                        groups.push((rest & 0x7f) as u8 | 0x80);
                        rest >>= 7;
                    }
                    plt.extend(groups.iter().rev());
                }
                segment(&mut header, PLT, &plt);
            }
            let body: u64 = packet_lengths.iter().sum();
            let psot = 12 + header.len() as u64 + 2 + body;
            cs.extend(SOT.to_be_bytes());
            cs.extend(10u16.to_be_bytes());
            cs.extend(0u16.to_be_bytes());
            cs.extend((psot as u32).to_be_bytes());
            cs.extend([0, self.tile_parts]);
            cs.extend(header);
            cs.extend(SOD.to_be_bytes());
            cs.extend(std::iter::repeat_n(0xaa, body as usize));
            cs.extend([0xff, 0xd9]);
            let mut file = b"\0\0\0\x0cjP  \r\n\x87\n".to_vec();
            file.extend(20u32.to_be_bytes());
            file.extend(b"ftypjp2 \0\0\0\0jp2 ");
            file.extend((cs.len() as u32 + 8).to_be_bytes());
            file.extend(b"jp2c");
            file.extend(cs);
            file
        }

        fn header_len(&self, file: &[u8]) -> usize {
            file.windows(2).position(|w| w == [0xff, 0x93]).unwrap() + 2
        }
    }

    fn ends(layout: Layout) -> ResolutionEnds {
        match layout {
            Layout::Resolutions(ends) => ends,
            other => panic!("expected resolutions, got {other:?}"),
        }
    }

    #[test]
    fn sums_packet_lengths_by_resolution() {
        // Default precincts: one per resolution, so 3 components x 2 layers each.
        let builder = Builder::default();
        let lengths: Vec<u64> = (1..=18).collect();
        let file = builder.build(&lengths);
        let start = builder.header_len(&file) as u64;
        let ends = ends(locate(&file, file.len() as u64));
        let sum = |n: usize| lengths[..n].iter().sum::<u64>();
        assert_eq!(
            ends.ends,
            vec![start + sum(6), start + sum(12), start + sum(18)]
        );
        assert_eq!(ends.tail_start, file.len() as u64 - 2);
        assert_eq!(ends.prefix_for(0), start + sum(18));
        assert_eq!(ends.prefix_for(2), start + sum(6));
        assert_eq!(ends.prefix_for(9), start + sum(6));
        // Only the header is needed; long lengths take several 7-bit groups.
        let long: Vec<u64> = (0..18).map(|i| 200 + i * 1000).collect();
        let file = builder.build(&long);
        let header = builder.header_len(&file);
        assert!(matches!(
            locate(&file[..header], file.len() as u64),
            Layout::Resolutions(_)
        ));
        assert_eq!(
            locate(&file[..header - 1], file.len() as u64),
            Layout::NeedMore
        );
    }

    #[test]
    fn counts_precincts_at_each_resolution() {
        // 1000 x 600 with 2 levels: resolutions 250 x 150, 500 x 300, 1000 x 600.
        // 128 px precincts at the lowest two (with exponents 7) give 2 x 2 and
        // 4 x 3; 256 px at the top give 4 x 3.
        let builder = Builder {
            components: 1,
            layers: 1,
            precincts: Some(vec![0x77, 0x77, 0x88]),
            ..Builder::default()
        };
        let lengths = vec![1u64; 4 + 12 + 12];
        let file = builder.build(&lengths);
        let start = builder.header_len(&file) as u64;
        assert_eq!(
            ends(locate(&file, file.len() as u64)).ends,
            vec![start + 4, start + 16, start + 28]
        );
        // One packet too few for the header's precincts.
        let file = builder.build(&lengths[1..]);
        assert_eq!(
            locate(&file, file.len() as u64),
            Layout::Unsupported("PLT packet count does not match the header")
        );
    }

    #[test]
    fn reads_plt_segments_in_zplt_order() {
        // Lengths that differ by resolution, so a reordering moves the cutoffs
        // without changing the packet count or total.
        let lengths: Vec<u64> = (0..18).map(|i| if i < 6 { 1 } else { 300 }).collect();
        let split = Builder {
            plt: vec![0, 1, 2],
            ..Builder::default()
        };
        let file = split.build(&lengths);
        let start = split.header_len(&file) as u64;
        assert_eq!(
            ends(locate(&file, file.len() as u64)).ends,
            vec![start + 6, start + 6 + 1800, start + 6 + 3600]
        );
        // Gaps in the sequence are allowed; j2k only sorts by it.
        let gaps = Builder {
            plt: vec![3, 7, 200],
            ..Builder::default()
        };
        let file = gaps.build(&lengths);
        assert!(matches!(
            locate(&file, file.len() as u64),
            Layout::Resolutions(_)
        ));
        // Segments out of order, or repeating an index (as one that wrapped
        // past 255 would), are decoded by j2k in another order.
        for plt in [vec![1, 0, 2], vec![0, 0, 1]] {
            let builder = Builder {
                plt,
                ..Builder::default()
            };
            let file = builder.build(&lengths);
            assert_eq!(
                locate(&file, file.len() as u64),
                Layout::Unsupported("PLT markers out of Zplt order")
            );
        }
    }

    #[test]
    fn rejects_a_packet_length_split_across_plt_segments() {
        let builder = Builder::default();
        let lengths = vec![200u64; 18];
        let mut file = builder.build(&lengths);
        // Turn the single PLT segment into two, cutting the first length
        // after its continuation byte. The marker adds 5 bytes to the header.
        let plt = file.windows(2).position(|w| w == [0xff, 0x58]).unwrap();
        let len = u16::from_be_bytes([file[plt + 2], file[plt + 3]]) as usize;
        file[plt + 2..plt + 4].copy_from_slice(&4u16.to_be_bytes());
        let rest = (len - 4) as u16 + 3;
        let mut second = vec![0xff, 0x58];
        second.extend(rest.to_be_bytes());
        second.push(1);
        file.splice(plt + 6..plt + 6, second);
        // Keep Psot and the jp2c box length consistent with the longer header.
        let sot = file.windows(2).position(|w| w == [0xff, 0x90]).unwrap();
        let psot = u32::from_be_bytes(file[sot + 6..sot + 10].try_into().unwrap()) + 5;
        file[sot + 6..sot + 10].copy_from_slice(&psot.to_be_bytes());
        let jp2c = file.windows(4).position(|w| w == b"jp2c").unwrap() - 4;
        let boxlen = u32::from_be_bytes(file[jp2c..jp2c + 4].try_into().unwrap()) + 5;
        file[jp2c..jp2c + 4].copy_from_slice(&boxlen.to_be_bytes());
        assert_eq!(
            locate(&file, file.len() as u64),
            Layout::Unsupported("invalid PLT")
        );
    }

    #[test]
    fn falls_back_for_unsupported_layouts() {
        let lengths: Vec<u64> = vec![1; 18];
        let cases = [
            (
                Builder {
                    plt: vec![],
                    ..Builder::default()
                },
                "no PLT markers",
            ),
            (
                Builder {
                    progression: LRCP,
                    ..Builder::default()
                },
                "progression is not resolution-major",
            ),
            (
                Builder {
                    tile_parts: 2,
                    ..Builder::default()
                },
                "more than one tile-part",
            ),
        ];
        for (builder, reason) in cases {
            let file = builder.build(&lengths);
            assert_eq!(
                locate(&file, file.len() as u64),
                Layout::Unsupported(reason)
            );
        }
        // LRCP with one layer sends resolutions in order.
        let builder = Builder {
            progression: LRCP,
            layers: 1,
            ..Builder::default()
        };
        let file = builder.build(&lengths[..9]);
        assert!(matches!(
            locate(&file, file.len() as u64),
            Layout::Resolutions(_)
        ));
        assert_eq!(
            locate(b"\xff\x0a not a jp2 at all", 100),
            Layout::Unsupported("not a JP2 with a codestream box")
        );
    }

    /// The property everything depends on: with only the bytes up to a level's
    /// cutoff and the tail on disk, the decode is exactly the full decode.
    #[test]
    fn decodes_each_level_exactly_from_its_cutoff() {
        // The validator image re-encoded by Kakadu as one RPCL tile-part with PLT
        // markers, 4 levels, 3 layers, and 128 then 64 px precincts.
        let file = std::fs::read("tests/fixtures/rpcl-plt.jp2").unwrap();
        let ends = ends(locate(&file, file.len() as u64));
        let full = crate::iiif::Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 1000,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut previous = 0;
        for level in (0..=4).rev() {
            let cut = ends.prefix_for(level);
            assert!(cut > previous, "each resolution adds packets");
            previous = cut;
            let mut partial = file.clone();
            partial[cut as usize..ends.tail_start as usize].fill(0);
            let path = dir.path().join(format!("level-{level}.jp2"));
            std::fs::write(&path, &partial).unwrap();
            let decode = |path: &std::path::Path, complete| {
                crate::j2k_decoder::decode_region(path, full, level, 1_000_000, complete)
                    .unwrap()
                    .into_bytes()
            };
            assert!(
                decode(&path, false)
                    == decode(std::path::Path::new("tests/fixtures/rpcl-plt.jp2"), true),
                "level {level} differs"
            );
        }
        assert_eq!(previous, ends.tail_start);
        // Only the codestream header is needed to work this out.
        let header = file.windows(2).position(|w| w == [0xff, 0x93]).unwrap() + 2;
        assert!(matches!(
            locate(&file[..header], file.len() as u64),
            Layout::Resolutions(_)
        ));
    }

    #[test]
    fn rejects_the_validator_fixture_split_into_tile_parts() {
        // Its first tile-part does not give the number of tile-parts, but it ends
        // well before the codestream does.
        let file = std::fs::read("tests/fixtures/iiif-validator.jp2").unwrap();
        assert_eq!(
            locate(&file, file.len() as u64),
            Layout::Unsupported("tile-part does not end the codestream")
        );
    }
}
