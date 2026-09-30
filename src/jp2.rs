use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

/// Read width, height, and component count from the JP2 image-header box.
/// Raw codestreams and JPX compositions are not part of the initial source contract.
pub fn metadata(path: &Path) -> anyhow::Result<(u32, u32, u16)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let jp2h = find_box(&mut file, 0, len, *b"jp2h")?
        .ok_or_else(|| anyhow::anyhow!("JP2 header box not found"))?;
    let ihdr = find_box(&mut file, jp2h.0, jp2h.1, *b"ihdr")?
        .ok_or_else(|| anyhow::anyhow!("JP2 image header box not found"))?;
    if ihdr.1 - ihdr.0 < 10 {
        anyhow::bail!("truncated JP2 image header");
    }
    file.seek(SeekFrom::Start(ihdr.0))?;
    let mut dims = [0u8; 10];
    file.read_exact(&mut dims)?;
    let height = u32::from_be_bytes(dims[..4].try_into()?);
    let width = u32::from_be_bytes(dims[4..8].try_into()?);
    let components = u16::from_be_bytes(dims[8..10].try_into()?);
    if width == 0 || height == 0 {
        anyhow::bail!("zero JP2 dimensions");
    }
    if components == 0 {
        anyhow::bail!("zero JP2 components");
    }
    Ok((width, height, components))
}

fn find_box(
    file: &mut File,
    start: u64,
    end: u64,
    wanted: [u8; 4],
) -> anyhow::Result<Option<(u64, u64)>> {
    let mut pos = start;
    while pos + 8 <= end {
        file.seek(SeekFrom::Start(pos))?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        let len32 = u32::from_be_bytes(header[..4].try_into()?);
        let kind: [u8; 4] = header[4..].try_into()?;
        let (box_len, header_len) = match len32 {
            0 => (end - pos, 8),
            1 => {
                let mut extended = [0u8; 8];
                file.read_exact(&mut extended)?;
                (u64::from_be_bytes(extended), 16)
            }
            _ => (u64::from(len32), 8),
        };
        if box_len < header_len || pos.checked_add(box_len).is_none_or(|next| next > end) {
            anyhow::bail!("invalid JP2 box length");
        }
        if kind == wanted {
            return Ok(Some((pos + header_len, pos + box_len)));
        }
        pos += box_len;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jp2");
        let mut data = Vec::new();
        data.extend([0, 0, 0, 26]);
        data.extend(b"jp2h");
        data.extend([0, 0, 0, 18]);
        data.extend(b"ihdr");
        data.extend(24u32.to_be_bytes());
        data.extend(48u32.to_be_bytes());
        data.extend(3u16.to_be_bytes());
        std::fs::write(&path, data).unwrap();
        assert_eq!(metadata(&path).unwrap(), (48, 24, 3));
    }
}
