use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1F && bytes[1] == 0x8B
}

pub fn open_maybe_gzip(path: &Path) -> io::Result<Box<dyn Read>> {
    let mut file = fs::File::open(path)?;
    let mut magic = [0u8; 2];
    let n = file.read(&mut magic)?;
    file.seek(SeekFrom::Start(0))?;
    if n == 2 && magic == [0x1F, 0x8B] {
        Ok(Box::new(GzDecoder::new(file)))
    } else {
        Ok(Box::new(file))
    }
}

pub fn read_prproj_xml(path: &Path) -> io::Result<(Vec<u8>, bool)> {
    let original = fs::read(path)?;
    let gzipped = is_gzip(&original);
    if gzipped {
        let mut decoder = GzDecoder::new(&original[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)?;
        Ok((decompressed, true))
    } else {
        Ok((original, false))
    }
}

pub fn write_prproj_xml(path: &Path, xml: &[u8], gzipped: bool) -> io::Result<()> {
    if gzipped {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(xml)?;
        let compressed = encoder.finish()?;
        fs::write(path, compressed)
    } else {
        fs::write(path, xml)
    }
}

pub fn skip_oversize(path: &Path, max_size_bytes: Option<usize>) -> io::Result<bool> {
    if let Some(max_bytes) = max_size_bytes {
        let metadata = fs::metadata(path)?;
        if metadata.len() > max_bytes as u64 {
            return Ok(true);
        }
    }
    Ok(false)
}
