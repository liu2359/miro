//! encode and decode the frames for the mux protocol.
//! The frames include the length of a PDU as well as an identifier
//! that informs us how to decode it.  The length, ident and serial
//! number are encoded using a variable length integer encoding.
//! Rather than rely solely on serde to serialize and deserialize an
//! enum, we encode the enum variants with a version/identifier tag
//! for ourselves.  This will make it a little easier to manage
//! client and server instances that are built from different versions
//! of this code; in this way the client and server can more gracefully
//! manage unknown enum variants.
#![allow(dead_code)]

use crate::core::hyperlink::Hyperlink;
use crate::core::surface::{Change, SequenceNo};
use crate::mux::domain::DomainId;
use crate::mux::tab::TabId;
use crate::mux::window::WindowId;
use crate::pty::{CommandBuilder, PtySize};
use crate::term::selection::SelectionRange;
use failure::{bail, Error, Fallible};
use leb128;
use log::debug;
use serde_derive::*;
use std::io::Cursor;
use std::sync::Arc;
use varbincode;

/// Returns the encoded length of the leb128 representation of value
fn encoded_length(value: u64) -> usize {
    struct NullWrite {};
    impl std::io::Write for NullWrite {
        fn write(&mut self, buf: &[u8]) -> std::result::Result<usize, std::io::Error> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::result::Result<(), std::io::Error> {
            Ok(())
        }
    };

    leb128::write::unsigned(&mut NullWrite {}, value).unwrap()
}

const COMPRESSED_MASK: u64 = 1 << 63;

/// Encode a frame.  If the data is compressed, the high bit of the length
/// is set to indicate that.  The data written out has the format:
/// tagged_len: leb128  (u64 msb is set if data is compressed)
/// serial: leb128
/// ident: leb128
/// data bytes
fn encode_raw<W: std::io::Write>(
    ident: u64,
    serial: u64,
    data: &[u8],
    is_compressed: bool,
    mut w: W,
) -> Result<(), std::io::Error> {
    let len = data.len() + encoded_length(ident) + encoded_length(serial);
    let masked_len = if is_compressed { (len as u64) | COMPRESSED_MASK } else { len as u64 };

    // Double-buffer the data; since we run with nodelay enabled, it is
    // desirable for the write to be a single packet (or at least, for
    // the header portion to go out in a single packet)
    let mut buffer = Vec::with_capacity(len + encoded_length(masked_len));

    leb128::write::unsigned(&mut buffer, masked_len)?;
    leb128::write::unsigned(&mut buffer, serial)?;
    leb128::write::unsigned(&mut buffer, ident)?;
    buffer.extend_from_slice(data);

    w.write_all(&buffer)
}

/// Read a single leb128 encoded value from the stream
fn read_u64<R: std::io::Read>(mut r: R) -> Result<u64, std::io::Error> {
    leb128::read::unsigned(&mut r).map_err(|err| match err {
        leb128::read::Error::IoError(ioerr) => ioerr,
        err => std::io::Error::new(std::io::ErrorKind::Other, format!("{}", err)),
    })
}

#[derive(Debug)]
struct Decoded {
    ident: u64,
    serial: u64,
    data: Vec<u8>,
    is_compressed: bool,
}

/// Decode a frame.
/// See encode_raw() for the frame format.
fn decode_raw<R: std::io::Read>(mut r: R) -> Result<Decoded, std::io::Error> {
    let len = read_u64(r.by_ref())?;
    let (len, is_compressed) =
        if (len & COMPRESSED_MASK) != 0 { (len & !COMPRESSED_MASK, true) } else { (len, false) };
    let serial = read_u64(r.by_ref())?;
    let ident = read_u64(r.by_ref())?;
    let data_len = len as usize - (encoded_length(ident) + encoded_length(serial));
    let mut data = vec![0u8; data_len];
    r.read_exact(&mut data)?;
    Ok(Decoded { ident, serial, data, is_compressed })
}

#[derive(Debug, PartialEq)]
pub struct DecodedPdu {
    pub serial: u64,
    pub pdu: Pdu,
}

/// If the serialized size is larger than this, then we'll consider compressing it
const COMPRESS_THRESH: usize = 32;

fn serialize<T: serde::Serialize>(t: &T) -> Result<(Vec<u8>, bool), Error> {
    let mut uncompressed = Vec::new();
    let mut encode = varbincode::Serializer::new(&mut uncompressed);
    t.serialize(&mut encode)?;

    if uncompressed.len() <= COMPRESS_THRESH {
        return Ok((uncompressed, false));
    }
    // It's a little heavy; let's try compressing it
    let mut compressed = Vec::new();
    let mut compress = zstd::Encoder::new(&mut compressed, zstd::DEFAULT_COMPRESSION_LEVEL)?;
    let mut encode = varbincode::Serializer::new(&mut compress);
    t.serialize(&mut encode)?;
    drop(encode);
    compress.finish()?;

    debug!("serialized+compress len {} vs {}", compressed.len(), uncompressed.len());

    if compressed.len() < uncompressed.len() {
        Ok((compressed, true))
    } else {
        Ok((uncompressed, false))
    }
}

fn deserialize<T: serde::de::DeserializeOwned, R: std::io::Read>(
    mut r: R,
    is_compressed: bool,
) -> Result<T, Error> {
    if is_compressed {
        let mut decompress = zstd::Decoder::new(r)?;
        let mut decode = varbincode::Deserializer::new(&mut decompress);
        serde::Deserialize::deserialize(&mut decode).map_err(Into::into)
    } else {
        let mut decode = varbincode::Deserializer::new(&mut r);
        serde::Deserialize::deserialize(&mut decode).map_err(Into::into)
    }
}

macro_rules! pdu {
    ($( $name:ident:$vers:expr),* $(,)?) => {
        #[derive(PartialEq, Debug)]
        pub enum Pdu {
            Invalid{ident: u64},
            $(
                $name($name)
            ,)*
        }

        impl Pdu {
            pub fn encode<W: std::io::Write>(&self, w: W, serial: u64) -> Result<(), Error> {
                match self {
                    Pdu::Invalid{..} => bail!("attempted to serialize Pdu::Invalid"),
                    $(
                        Pdu::$name(s) => {
                            let (data, is_compressed) = serialize(s)?;
                            encode_raw($vers, serial, &data, is_compressed, w)?;
                            Ok(())
                        }
                    ,)*
                }
            }

            pub fn decode<R: std::io::Read>(r:R) -> Result<DecodedPdu, Error> {
                let decoded = decode_raw(r)?;
                match decoded.ident {
                    $(
                        $vers => {
                            Ok(DecodedPdu {
                                serial: decoded.serial,
                                pdu: Pdu::$name(deserialize(decoded.data.as_slice(), decoded.is_compressed)?)
                            })
                        }
                    ,)*
                    _ => Ok(DecodedPdu {
                        serial: decoded.serial,
                        pdu: Pdu::Invalid{ident:decoded.ident}
                    }),
                }
            }
        }
    }
}

// Defines the Pdu enum.
// Each struct has an explicit identifying number.
// This allows removal of obsolete structs,
// and defining newer structs as the protocol evolves.
pdu! {
    ErrorResponse: 0,
    Ping: 1,
    Pong: 2,
    ListTabs: 3,
    ListTabsResponse: 4,
    Spawn: 7,
    SpawnResponse: 8,
    WriteToTab: 9,
    UnitResponse: 10,
    SendKeyDown: 11,
    SendMouseEvent: 12,
    SendPaste: 13,
    Resize: 14,
    SendMouseEventResponse: 17,
    GetTabRenderChanges: 18,
    GetTabRenderChangesResponse: 19,
    SetClipboard: 20,
    OpenURL: 21,
}

impl Pdu {
    pub fn stream_decode(buffer: &mut Vec<u8>) -> Fallible<Option<DecodedPdu>> {
        let mut cursor = Cursor::new(buffer.as_slice());
        match Self::decode(&mut cursor) {
            Ok(decoded) => {
                let consumed = cursor.position() as usize;
                let remain = buffer.len() - consumed;
                // Remove `consumed` bytes from the start of the vec.
                // This is safe because the vec is just bytes and we are
                // constrained the offsets accordingly.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        buffer.as_ptr().add(consumed),
                        buffer.as_mut_ptr(),
                        remain,
                    );
                }
                buffer.truncate(remain);
                Ok(Some(decoded))
            }
            Err(err) => {
                if let Some(ioerr) = err.downcast_ref::<std::io::Error>() {
                    match ioerr.kind() {
                        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::WouldBlock => {
                            return Ok(None);
                        }
                        _ => {}
                    }
                }
                Err(err)
            }
        }
    }

    pub fn try_read_and_decode<R: std::io::Read>(
        r: &mut R,
        buffer: &mut Vec<u8>,
    ) -> Fallible<Option<DecodedPdu>> {
        loop {
            if let Some(decoded) = Self::stream_decode(buffer)? {
                return Ok(Some(decoded));
            }

            let mut buf = [0u8; 4096];
            let size = match r.read(&mut buf) {
                Ok(size) => size,
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::WouldBlock {
                        return Ok(None);
                    }
                    return Err(err.into());
                }
            };
            if size == 0 {
                return Err(
                    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "End Of File").into()
                );
            }

            buffer.extend_from_slice(&buf[0..size]);
        }
    }

    pub fn tab_id(&self) -> Option<TabId> {
        match self {
            Pdu::GetTabRenderChangesResponse(GetTabRenderChangesResponse { tab_id, .. }) => {
                Some(*tab_id)
            }
            Pdu::SetClipboard(SetClipboard { tab_id, .. }) => Some(*tab_id),
            Pdu::OpenURL(OpenURL { tab_id, .. }) => Some(*tab_id),
            _ => None,
        }
    }
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct UnitResponse {}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct ErrorResponse {
    pub reason: String,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct Ping {}
#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct Pong {}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct ListTabs {}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct WindowAndTabEntry {
    pub window_id: WindowId,
    pub tab_id: TabId,
    pub title: String,
    pub size: PtySize,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct ListTabsResponse {
    pub tabs: Vec<WindowAndTabEntry>,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct Spawn {
    pub domain_id: DomainId,
    /// If None, create a new window for this new tab
    pub window_id: Option<WindowId>,
    pub command: Option<CommandBuilder>,
    pub size: PtySize,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct SpawnResponse {
    pub tab_id: TabId,
    pub window_id: WindowId,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct WriteToTab {
    pub tab_id: TabId,
    pub data: Vec<u8>,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct SendPaste {
    pub tab_id: TabId,
    pub data: String,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct SendKeyDown {
    pub tab_id: TabId,
    pub event: crate::core::input::KeyEvent,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct SendMouseEvent {
    pub tab_id: TabId,
    pub event: crate::term::input::MouseEvent,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct SendMouseEventResponse {
    pub selection_range: Option<SelectionRange>,
    pub highlight: Option<Arc<Hyperlink>>,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct SetClipboard {
    pub tab_id: TabId,
    pub clipboard: Option<String>,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct OpenURL {
    pub tab_id: TabId,
    pub url: String,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct Resize {
    pub tab_id: TabId,
    pub size: PtySize,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct GetTabRenderChanges {
    pub tab_id: TabId,
    pub sequence_no: SequenceNo,
}

#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct GetTabRenderChangesResponse {
    pub tab_id: TabId,
    pub sequence_no: SequenceNo,
    pub changes: Vec<Change>,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_frame() {
        let mut encoded = Vec::new();
        encode_raw(0x81, 0x42, b"hello", false, &mut encoded).unwrap();
        assert_eq!(&encoded, b"\x08\x42\x81\x01hello");
        let decoded = decode_raw(encoded.as_slice()).unwrap();
        assert_eq!(decoded.ident, 0x81);
        assert_eq!(decoded.serial, 0x42);
        assert_eq!(decoded.data, b"hello");
    }

    #[test]
    fn test_frame_lengths() {
        let mut serial = 1;
        for target_len in &[128, 247, 256, 65536, 16777216] {
            let mut payload = Vec::with_capacity(*target_len);
            payload.resize(*target_len, b'a');
            let mut encoded = Vec::new();
            encode_raw(0x42, serial, payload.as_slice(), false, &mut encoded).unwrap();
            let decoded = decode_raw(encoded.as_slice()).unwrap();
            assert_eq!(decoded.ident, 0x42);
            assert_eq!(decoded.serial, serial);
            assert_eq!(decoded.data, payload);
            serial += 1;
        }
    }

    #[test]
    fn test_pdu_ping() {
        let mut encoded = Vec::new();
        Pdu::Ping(Ping {}).encode(&mut encoded, 0x40).unwrap();
        assert_eq!(&encoded, &[2, 0x40, 1]);
        assert_eq!(
            DecodedPdu { serial: 0x40, pdu: Pdu::Ping(Ping {}) },
            Pdu::decode(encoded.as_slice()).unwrap()
        );
    }

    #[test]
    fn stream_decode() {
        let mut encoded = Vec::new();
        Pdu::Ping(Ping {}).encode(&mut encoded, 0x1).unwrap();
        Pdu::Pong(Pong {}).encode(&mut encoded, 0x2).unwrap();
        assert_eq!(encoded.len(), 6);

        let mut cursor = Cursor::new(encoded.as_slice());
        let mut read_buffer = Vec::new();

        assert_eq!(
            Pdu::try_read_and_decode(&mut cursor, &mut read_buffer).unwrap(),
            Some(DecodedPdu { serial: 1, pdu: Pdu::Ping(Ping {}) })
        );
        assert_eq!(
            Pdu::try_read_and_decode(&mut cursor, &mut read_buffer).unwrap(),
            Some(DecodedPdu { serial: 2, pdu: Pdu::Pong(Pong {}) })
        );
        let err = Pdu::try_read_and_decode(&mut cursor, &mut read_buffer).unwrap_err();
        assert_eq!(
            err.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn test_pdu_ping_base91() {
        let mut encoded = Vec::new();
        {
            let mut encoder = crate::core::base91::Base91Encoder::new(&mut encoded);
            Pdu::Ping(Ping {}).encode(&mut encoder, 0x41).unwrap();
        }
        assert_eq!(&encoded, &[60, 67, 75, 65]);
        let decoded = crate::core::base91::decode(&encoded);
        assert_eq!(
            DecodedPdu { serial: 0x41, pdu: Pdu::Ping(Ping {}) },
            Pdu::decode(decoded.as_slice()).unwrap()
        );
    }

    #[test]
    fn test_pdu_pong() {
        let mut encoded = Vec::new();
        Pdu::Pong(Pong {}).encode(&mut encoded, 0x42).unwrap();
        assert_eq!(&encoded, &[2, 0x42, 2]);
        assert_eq!(
            DecodedPdu { serial: 0x42, pdu: Pdu::Pong(Pong {}) },
            Pdu::decode(encoded.as_slice()).unwrap()
        );
    }

    #[test]
    fn test_bogus_pdu() {
        let mut encoded = Vec::new();
        encode_raw(0xdeadbeef, 0x42, b"hello", false, &mut encoded).unwrap();
        assert_eq!(
            DecodedPdu { serial: 0x42, pdu: Pdu::Invalid { ident: 0xdeadbeef } },
            Pdu::decode(encoded.as_slice()).unwrap()
        );
    }
}
