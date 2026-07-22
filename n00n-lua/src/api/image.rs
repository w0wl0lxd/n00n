//! `n00n.image`: image primitives for plugins. Neovim has no image API to
//! mirror, so these are small blocks (probe, decode, resize, encode) that
//! plugins compose; provider policy stays in Lua. Errors follow the
//! `(nil, err)` convention of `n00n.fs`.

use std::io::{Cursor, Error, ErrorKind, Result as IoResult, Seek, SeekFrom, Write};
use std::sync::Arc;

use image::{DynamicImage, ImageFormat, ImageReader};
use mlua::{Lua, Result as LuaResult, Value as LuaValue};
use n00n_lua_macro::{lua_class, lua_fn, lua_table};

use super::base64::bytes_arg;

/// Decode-bomb guard: a tiny file can declare huge dimensions and balloon
/// into gigabytes of RGBA. Host-fixed so no plugin can disable it; 50MP
/// still covers any real camera photo.
const MAX_PIXELS: u64 = 50_000_000;
/// A tile is base64-encoded by the caller, so raw output must stay below the
/// shared 5 MB transport ceiling. This also caps encoder allocation.
const MAX_ENCODED_BYTES: usize = 3_750_000;

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        // Only jpeg deviates from its primary extension ("jpg").
        ImageFormat::Jpeg => "jpeg",
        other => other
            .extensions_str()
            .first()
            .copied()
            .unwrap_or_else(|| "unknown"),
    }
}

fn probe_bytes(bytes: &[u8]) -> Result<(ImageFormat, u32, u32), String> {
    let format =
        image::guess_format(bytes).map_err(|_| "not an image (unrecognized format)".to_owned())?;
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|e| format!("cannot read image header: {e}"))?;
    Ok((format, width, height))
}

fn gif_is_animated(bytes: &[u8]) -> bool {
    if bytes.len() < 13 || (&bytes[..6] != b"GIF87a" && &bytes[..6] != b"GIF89a") {
        return false;
    }
    let mut pos: usize = 13;
    if bytes[10] & 0x80 != 0 {
        let entries = 1_usize << (usize::from(bytes[10] & 0x07) + 1);
        let Some(table_len) = entries.checked_mul(3) else {
            return false;
        };
        let Some(next) = pos.checked_add(table_len) else {
            return false;
        };
        if next > bytes.len() {
            return false;
        }
        pos = next;
    }

    let mut frames = 0_u8;
    while let Some(&block) = bytes.get(pos) {
        pos += 1;
        match block {
            0x2C => {
                let Some(descriptor_end) = pos.checked_add(9) else {
                    return false;
                };
                if descriptor_end > bytes.len() {
                    return false;
                }
                frames = frames.saturating_add(1);
                if frames >= 2 {
                    return true;
                }
                let packed = bytes[pos + 8];
                pos = descriptor_end;
                if packed & 0x80 != 0 {
                    let entries = 1_usize << (usize::from(packed & 0x07) + 1);
                    let Some(table_len) = entries.checked_mul(3) else {
                        return false;
                    };
                    let Some(next) = pos.checked_add(table_len) else {
                        return false;
                    };
                    if next > bytes.len() {
                        return false;
                    }
                    pos = next;
                }
                if pos >= bytes.len() {
                    return false;
                }
                pos += 1;
                let Some(next) = skip_gif_sub_blocks(bytes, pos) else {
                    return false;
                };
                pos = next;
            }
            0x21 => {
                if pos >= bytes.len() {
                    return false;
                }
                pos += 1;
                let Some(next) = skip_gif_sub_blocks(bytes, pos) else {
                    return false;
                };
                pos = next;
            }
            _ => return false,
        }
    }
    false
}

fn skip_gif_sub_blocks(bytes: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = usize::from(*bytes.get(pos)?);
        pos = pos.checked_add(1)?;
        if len == 0 {
            return Some(pos);
        }
        pos = pos.checked_add(len)?;
        (pos <= bytes.len()).then_some(pos)?;
    }
}

fn webp_is_animated(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return false;
    }
    let mut pos: usize = 12;
    while let Some(header_end) = pos.checked_add(8) {
        if header_end > bytes.len() {
            return false;
        }
        let kind = &bytes[pos..pos + 4];
        if kind == b"ANIM" || kind == b"ANMF" {
            return true;
        }
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]);
        let Ok(size) = usize::try_from(size) else {
            return false;
        };
        let Some(next) = header_end
            .checked_add(size)
            .and_then(|v| v.checked_add(size & 1))
        else {
            return false;
        };
        if next > bytes.len() {
            return false;
        }
        pos = next;
    }
    false
}

fn is_animated(format: ImageFormat, bytes: &[u8]) -> bool {
    match format {
        ImageFormat::Gif => gif_is_animated(bytes),
        ImageFormat::WebP => webp_is_animated(bytes),
        _ => false,
    }
}

fn decode_bytes(bytes: &[u8]) -> Result<DynamicImage, String> {
    let (format, width, height) = probe_bytes(bytes)?;
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(format!(
            "image too large to decode ({width}x{height}; limit {MAX_PIXELS} pixels)"
        ));
    }
    image::load_from_memory_with_format(bytes, format).map_err(|e| format!("cannot decode: {e}"))
}

/// Opaque decoded image. `Arc` so resize/encode can hop to a blocking thread
/// without copying pixels.
struct LuaImage(Arc<DynamicImage>);

struct LimitedWriter {
    inner: Cursor<Vec<u8>>,
    max_bytes: usize,
}

impl LimitedWriter {
    fn new(max_bytes: usize) -> Self {
        Self {
            inner: Cursor::new(Vec::with_capacity(max_bytes.min(64 * 1024))),
            max_bytes,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.inner.into_inner()
    }
}

impl Write for LimitedWriter {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        let pos = usize::try_from(self.inner.position()).map_err(|_| {
            Error::new(
                ErrorKind::InvalidInput,
                "encoded image position is too large",
            )
        })?;
        let end = pos
            .checked_add(buf.len())
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "encoded image length overflows"))?;
        if end > self.max_bytes {
            return Err(Error::new(
                ErrorKind::WriteZero,
                "encoded image exceeds configured byte limit",
            ));
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> IoResult<()> {
        self.inner.flush()
    }
}

impl Seek for LimitedWriter {
    fn seek(&mut self, pos: SeekFrom) -> IoResult<u64> {
        self.inner.seek(pos)
    }
}

/// Get the width of the image in pixels.
///
/// @return (integer) Width in pixels.
#[lua_fn]
fn width(_lua: &Lua, this: &LuaImage) -> LuaResult<u32> {
    Ok(this.0.width())
}

/// Get the height of the image in pixels.
///
/// @return (integer) Height in pixels.
#[lua_fn]
fn height(_lua: &Lua, this: &LuaImage) -> LuaResult<u32> {
    Ok(this.0.height())
}

/// Shrink the image to fit inside {max_w} x {max_h}, keeping the aspect
/// ratio. If the image already fits, it is returned as-is. Never upscales.
///
/// @param max_w integer Maximum width in pixels. Must be positive.
/// @param max_h integer Maximum height in pixels. Must be positive.
/// @return (n00n.image.Image) A new image handle (or the same one if no resize was needed).
/// @example
/// local img = n00n.image.decode(raw_bytes)
/// local small = img:resize(800, 600)
/// local encoded = small:encode("jpeg")
#[lua_fn]
async fn resize(
    _lua: Lua,
    this: mlua::UserDataRef<LuaImage>,
    max_w: u32,
    max_h: u32,
) -> LuaResult<LuaImage> {
    let img = Arc::clone(&this.0);
    drop(this);
    if max_w == 0 || max_h == 0 {
        return Err(mlua::Error::runtime("resize: dimensions must be positive"));
    }
    if img.width() <= max_w && img.height() <= max_h {
        return Ok(LuaImage(img));
    }
    let resized =
        smol::unblock(move || img.resize(max_w, max_h, image::imageops::FilterType::Triangle))
            .await;
    Ok(LuaImage(Arc::new(resized)))
}

/// Copy a rectangular pixel region without resizing it. Coordinates are
/// zero-based source pixels. The original image is unchanged.
///
/// @param x integer Left edge in source pixels.
/// @param y integer Top edge in source pixels.
/// @param width integer Crop width in pixels. Must be positive.
/// @param height integer Crop height in pixels. Must be positive.
/// @return (n00n.image.Image) A new image handle containing the crop.
/// @example
/// local tile = img:crop(0, 2000, 1440, 2000)
/// local png = tile:encode("png")
#[lua_fn]
async fn crop(
    _lua: Lua,
    this: mlua::UserDataRef<LuaImage>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> LuaResult<LuaImage> {
    let img = Arc::clone(&this.0);
    drop(this);
    if width == 0 || height == 0 {
        return Err(mlua::Error::runtime("crop: dimensions must be positive"));
    }
    let right = x
        .checked_add(width)
        .ok_or_else(|| mlua::Error::runtime("crop: horizontal bounds overflow"))?;
    let bottom = y
        .checked_add(height)
        .ok_or_else(|| mlua::Error::runtime("crop: vertical bounds overflow"))?;
    if right > img.width() || bottom > img.height() {
        return Err(mlua::Error::runtime(format!(
            "crop: bounds x=[{x},{right}) y=[{y},{bottom}) are outside {}x{} image",
            img.width(),
            img.height()
        )));
    }
    let cropped = smol::unblock(move || img.crop_imm(x, y, width, height)).await;
    Ok(LuaImage(Arc::new(cropped)))
}

/// Encode the image into raw bytes in the given format. Use this to prepare
/// images for sending over the network or writing to disk.
///
/// @param format string Output format: `"png"`, `"jpeg"`, or `"jpg"`.
/// @return (string) Encoded image bytes.
/// @example
/// local bytes = img:encode("png")
/// -- bytes is a Lua string containing the raw PNG data
#[lua_fn]
async fn encode(
    lua: Lua,
    this: mlua::UserDataRef<LuaImage>,
    format: String,
) -> LuaResult<mlua::String> {
    let img = Arc::clone(&this.0);
    drop(this);
    let out_format = match format.as_str() {
        "png" => ImageFormat::Png,
        "jpeg" | "jpg" => ImageFormat::Jpeg,
        other => {
            return Err(mlua::Error::runtime(format!(
                "encode: unsupported format '{other}' (png, jpeg)"
            )));
        }
    };
    let encoded = smol::unblock(move || {
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), out_format)
            .map(|()| out)
    })
    .await
    .map_err(|e| mlua::Error::runtime(format!("encode: {e}")))?;
    lua.create_string(encoded)
}

/// Encode with a strict output-byte limit. Use this for untrusted or
/// transport-bound image output so encoding cannot grow a `Vec` without bound.
///
/// @param format string Output format: `"png"`, `"jpeg"`, or `"jpg"`.
/// @param max_bytes integer Maximum encoded bytes, up to 3,750,000.
/// @return (string?, string?) Encoded bytes, or nil plus an error when the limit is exceeded.
#[lua_fn]
async fn encode_limited(
    lua: Lua,
    this: mlua::UserDataRef<LuaImage>,
    format: String,
    max_bytes: u32,
) -> LuaResult<(LuaValue, LuaValue)> {
    if max_bytes == 0 {
        return Ok((
            LuaValue::Nil,
            LuaValue::String(lua.create_string("encode_limited: max_bytes must be positive")?),
        ));
    }
    let max_bytes = usize::try_from(max_bytes)
        .map_err(|_| mlua::Error::runtime("encode_limited: max_bytes is too large"))?;
    if max_bytes > MAX_ENCODED_BYTES {
        return Ok((
            LuaValue::Nil,
            LuaValue::String(lua.create_string(format!(
                "encode_limited: max_bytes exceeds host limit {MAX_ENCODED_BYTES}"
            ))?),
        ));
    }
    let out_format = match format.as_str() {
        "png" => ImageFormat::Png,
        "jpeg" | "jpg" => ImageFormat::Jpeg,
        other => {
            return Ok((
                LuaValue::Nil,
                LuaValue::String(lua.create_string(format!(
                    "encode_limited: unsupported format '{other}' (png, jpeg)"
                ))?),
            ));
        }
    };
    let img = Arc::clone(&this.0);
    drop(this);
    let result = smol::unblock(move || {
        let mut writer = LimitedWriter::new(max_bytes);
        img.write_to(&mut writer, out_format)
            .map(|()| writer.into_inner())
    })
    .await;
    match result {
        Ok(encoded) => Ok((LuaValue::String(lua.create_string(encoded)?), LuaValue::Nil)),
        Err(error) => Ok((
            LuaValue::Nil,
            LuaValue::String(lua.create_string(format!("encode_limited: {error}"))?),
        )),
    }
}

lua_class! {
    /// A decoded image you can inspect, resize, crop, and re-encode.
    ///
    /// Get one from `n00n.image.decode()`. The image data lives in memory
    /// until the handle is garbage collected.
    "n00n.image.Image" => LuaImage, IMAGE_DOCS [width, height, resize, crop, encode, encode_limited]
}

/// Read image metadata (format, dimensions) from raw bytes without fully
/// decoding the pixels. Much faster than `decode` when you only need to
/// check the size or format.
///
/// Returns a table with `format` (string), `width` (integer), `height`
/// (integer), and `animated` (boolean; GIF/WebP streams with multiple frames),
/// or `(nil, err)` if the bytes are not a recognized image.
///
/// @param data string|buffer Raw image bytes.
/// @return (table?, string?) Info table, or `(nil, err)` on failure.
/// @example
/// local info, err = n00n.image.probe(raw_bytes)
/// if err then error(err) end
/// print(info.format, info.width, info.height)
#[lua_fn]
fn probe(lua: &Lua, data: LuaValue) -> LuaResult<(LuaValue, LuaValue)> {
    let bytes = bytes_arg(&data, "image.probe")?;
    match probe_bytes(&bytes) {
        Ok((format, width, height)) => {
            let info = lua.create_table()?;
            info.set("format", format_name(format))?;
            info.set("width", width)?;
            info.set("height", height)?;
            info.set("animated", is_animated(format, &bytes))?;
            Ok((LuaValue::Table(info), LuaValue::Nil))
        }
        Err(e) => Ok((LuaValue::Nil, LuaValue::String(lua.create_string(e)?))),
    }
}

/// Decode raw image bytes into an Image handle you can resize and re-encode.
/// Images larger than 50 megapixels are rejected to prevent memory bombs.
///
/// @param data string|buffer Raw image bytes.
/// @return (n00n.image.Image?, string?) Decoded image, or `(nil, err)` on failure.
/// @example
/// local img, err = n00n.image.decode(raw_bytes)
/// if err then error(err) end
/// print(img:width() .. "x" .. img:height())
#[lua_fn]
async fn decode(lua: Lua, data: LuaValue) -> LuaResult<(LuaValue, LuaValue)> {
    let bytes = bytes_arg(&data, "image.decode")?;
    match smol::unblock(move || decode_bytes(&bytes)).await {
        Ok(img) => Ok((
            LuaValue::UserData(lua.create_userdata(LuaImage(Arc::new(img)))?),
            LuaValue::Nil,
        )),
        Err(e) => Ok((LuaValue::Nil, LuaValue::String(lua.create_string(e)?))),
    }
}

lua_table! {
    /// Small building blocks for working with images: probe metadata, decode
    /// pixels, resize, and encode back to bytes. Plugins compose these freely.
    ///
    /// Decoding is guarded against pixel-bomb attacks (50 MP limit).
    ///
    /// ```lua
    /// local img = n00n.image.decode(raw_bytes)
    /// local small = img:resize(1024, 768)
    /// local png = small:encode("png")
    /// ```
    "n00n.image" => pub(crate) fn create_image_table(), DOCS [
        probe, decode,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Lua-facing behavior is covered by plugins/view_image/tests/spec.lua
    // and the view_image tests in plugin_host.rs; only the host-side
    // decode-bomb guard is tested here.

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let img = DynamicImage::new_rgb8(width, height);
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFF_u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = (crc >> 1) ^ ((crc & 1) * 0xEDB8_8320);
            }
        }
        !crc
    }

    #[test]
    fn decode_rejects_pixel_bomb_before_allocating() {
        // Patch a real 1x1 PNG's IHDR to claim 10000x10000 and fix the CRC.
        // The cap must trip on the header alone, before any allocation.
        let mut bytes = png_bytes(1, 1);
        bytes[16..20].copy_from_slice(&10_000_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&10_000_u32.to_be_bytes());
        let crc = crc32(&bytes[12..29]);
        bytes[29..33].copy_from_slice(&crc.to_be_bytes());

        let (_, w, h) = probe_bytes(&bytes).expect("probe reads only the header");
        assert_eq!((w, h), (10_000, 10_000));
        let err = decode_bytes(&bytes).unwrap_err();
        assert!(err.contains("too large to decode"), "got: {err}");
    }
}
