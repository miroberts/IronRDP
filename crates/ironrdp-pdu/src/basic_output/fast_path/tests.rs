use std::sync::LazyLock;

use ironrdp_core::{decode, encode};

use super::*;

const FAST_PATH_HEADER_WITH_SHORT_LEN_BUFFER: [u8; 2] = [0x80, 0x08];
const FAST_PATH_HEADER_WITH_LONG_LEN_BUFFER: [u8; 3] = [0x80, 0x81, 0xE7];
const FAST_PATH_UPDATE_PDU_BUFFER: [u8; 19] = [
    0x4, 0x10, 0x0, 0x4, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x4, 0x0, 0x1, 0x0, 0x0, 0x0, 0x0, 0x0,
];
const FAST_PATH_UPDATE_PDU_WITH_LONG_LEN_BUFFER: [u8; 19] = [
    0x4, 0xff, 0x0, 0x4, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x4, 0x0, 0x1, 0x0, 0x0, 0x0, 0x0, 0x0,
];
const FAST_PATH_HEADER_WITH_FORCED_LONG_LEN_BUFFER: [u8; 3] = [0x80, 0x80, 0x08];

const FAST_PATH_HEADER_WITH_SHORT_LEN_PDU: FastPathHeader = FastPathHeader {
    flags: EncryptionFlags::ENCRYPTED,
    data_length: 6,
    forced_long_length: false,
};
const FAST_PATH_HEADER_WITH_LONG_LEN_PDU: FastPathHeader = FastPathHeader {
    flags: EncryptionFlags::ENCRYPTED,
    data_length: 484,
    forced_long_length: false,
};
const FAST_PATH_HEADER_WITH_FORCED_LONG_LEN_PDU: FastPathHeader = FastPathHeader {
    flags: EncryptionFlags::ENCRYPTED,
    data_length: 5,
    forced_long_length: true,
};

static FAST_PATH_UPDATE_PDU: LazyLock<FastPathUpdatePdu<'static>> = LazyLock::new(|| FastPathUpdatePdu {
    fragmentation: Fragmentation::Single,
    update_code: UpdateCode::SurfaceCommands,
    compression_flags: None,
    compression_type: None,
    data: &FAST_PATH_UPDATE_PDU_BUFFER[3..],
});

#[test]
fn from_buffer_correctly_parses_fast_path_header_with_short_length() {
    assert_eq!(
        FAST_PATH_HEADER_WITH_SHORT_LEN_PDU,
        decode::<FastPathHeader>(FAST_PATH_HEADER_WITH_SHORT_LEN_BUFFER.as_ref()).unwrap()
    );
}

#[test]
fn to_buffer_correctly_serializes_fast_path_header_with_short_length() {
    let expected = FAST_PATH_HEADER_WITH_SHORT_LEN_BUFFER.as_ref();
    let mut buffer = vec![0; expected.len()];

    encode(&FAST_PATH_HEADER_WITH_SHORT_LEN_PDU, buffer.as_mut_slice()).unwrap();
    assert_eq!(expected, buffer.as_slice());
}

#[test]
fn buffer_length_is_correct_for_fast_path_header_with_short_length() {
    assert_eq!(
        FAST_PATH_HEADER_WITH_SHORT_LEN_BUFFER.len(),
        FAST_PATH_HEADER_WITH_SHORT_LEN_PDU.size()
    );
}

#[test]
fn from_buffer_correctly_parses_fast_path_header_with_long_length() {
    assert_eq!(
        FAST_PATH_HEADER_WITH_LONG_LEN_PDU,
        decode::<FastPathHeader>(FAST_PATH_HEADER_WITH_LONG_LEN_BUFFER.as_ref()).unwrap()
    );
}

#[test]
fn to_buffer_correctly_serializes_fast_path_header_with_long_length() {
    let expected = FAST_PATH_HEADER_WITH_LONG_LEN_BUFFER.as_ref();
    let mut buffer = vec![0; expected.len()];

    encode(&FAST_PATH_HEADER_WITH_LONG_LEN_PDU, buffer.as_mut_slice()).unwrap();
    assert_eq!(expected, buffer.as_slice());
}

#[test]
fn buffer_length_is_correct_for_fast_path_header_with_long_length() {
    assert_eq!(
        FAST_PATH_HEADER_WITH_LONG_LEN_BUFFER.len(),
        FAST_PATH_HEADER_WITH_LONG_LEN_PDU.size()
    );
}

#[test]
fn from_buffer_correctly_parses_fast_path_header_with_forced_long_length() {
    assert_eq!(
        FAST_PATH_HEADER_WITH_FORCED_LONG_LEN_PDU,
        decode::<FastPathHeader>(FAST_PATH_HEADER_WITH_FORCED_LONG_LEN_BUFFER.as_ref()).unwrap()
    );
}

#[test]
fn to_buffer_correctly_serializes_fast_path_header_with_forced_long_length() {
    let expected = FAST_PATH_HEADER_WITH_FORCED_LONG_LEN_BUFFER.as_ref();
    let mut buffer = vec![0; expected.len()];

    encode(&FAST_PATH_HEADER_WITH_FORCED_LONG_LEN_PDU, buffer.as_mut_slice()).unwrap();
    assert_eq!(expected, buffer.as_slice());
}

#[test]
fn buffer_length_is_correct_for_fast_path_header_with_forced_long_length() {
    assert_eq!(
        FAST_PATH_HEADER_WITH_FORCED_LONG_LEN_BUFFER.len(),
        FAST_PATH_HEADER_WITH_FORCED_LONG_LEN_PDU.size()
    );
}

#[test]
fn from_buffer_correctly_parses_fast_path_update() {
    assert_eq!(
        *FAST_PATH_UPDATE_PDU,
        decode::<FastPathUpdatePdu<'_>>(FAST_PATH_UPDATE_PDU_BUFFER.as_ref()).unwrap()
    );
}

#[test]
fn from_buffer_returns_error_on_long_length_for_fast_path_update() {
    assert!(decode::<FastPathUpdatePdu<'_>>(FAST_PATH_UPDATE_PDU_WITH_LONG_LEN_BUFFER.as_ref()).is_err());
}

#[test]
fn to_buffer_correctly_serializes_fast_path_update() {
    let expected = FAST_PATH_UPDATE_PDU_BUFFER.as_ref();
    let mut buffer = vec![0; expected.len()];

    encode(&*FAST_PATH_UPDATE_PDU, buffer.as_mut_slice()).unwrap();
    assert_eq!(expected, buffer.as_slice());
}

#[test]
fn buffer_length_is_correct_for_fast_path_update() {
    assert_eq!(FAST_PATH_UPDATE_PDU_BUFFER.len(), FAST_PATH_UPDATE_PDU.size());
}

#[test]
fn buffer_size_boundary_fast_path_update() {
    let fph = FastPathHeader {
        flags: EncryptionFlags::ENCRYPTED,
        data_length: 125,
        forced_long_length: false,
    };
    assert_eq!(fph.size(), 2);
    let fph = FastPathHeader {
        flags: EncryptionFlags::ENCRYPTED,
        data_length: 126,
        forced_long_length: false,
    };
    assert_eq!(fph.size(), 3);
}

#[test]
fn decode_fast_path_palette_update() {
    // Fast-path palette with 3 colors
    let data: [u8; 17] = [
        0x02, 0x00, // UPDATETYPE_PALETTE
        0x00, 0x00, // padding
        0x03, 0x00, 0x00, 0x00, // numberColors = 3
        0xFF, 0x00, 0x00, // Red
        0x00, 0xFF, 0x00, // Green
        0x00, 0x00, 0xFF, // Blue
    ];

    let palette = FastPathUpdate::decode_with_code(&data, UpdateCode::Palette).unwrap();
    if let FastPathUpdate::Palette(p) = palette {
        assert_eq!(p.entries.len(), 3);
        assert_eq!(
            p.entries[0],
            PaletteEntry {
                red: 0xFF,
                green: 0x00,
                blue: 0x00
            }
        );
        assert_eq!(
            p.entries[1],
            PaletteEntry {
                red: 0x00,
                green: 0xFF,
                blue: 0x00
            }
        );
        assert_eq!(
            p.entries[2],
            PaletteEntry {
                red: 0x00,
                green: 0x00,
                blue: 0xFF
            }
        );
    } else {
        panic!("Expected Palette update");
    }
}

#[test]
fn compressed_update_round_trips() {
    // The encoder must set the COMPRESSION_USED bit on the update header when
    // compression flags are present, otherwise the decoder does not consume the
    // trailing compression flags byte and misreads the data length.
    let data = [0xAAu8; 8];
    let pdu = FastPathUpdatePdu {
        fragmentation: Fragmentation::Single,
        update_code: UpdateCode::SurfaceCommands,
        compression_flags: Some(CompressionFlags::COMPRESSED),
        compression_type: Some(CompressionType::K64),
        data: &data,
    };

    let mut buffer = vec![0u8; pdu.size()];
    encode(&pdu, buffer.as_mut_slice()).unwrap();

    assert_eq!(pdu, decode::<FastPathUpdatePdu<'_>>(&buffer).unwrap());
}

#[test]
fn encode_fast_path_palette_update() {
    use ironrdp_core::WriteCursor;

    let palette = FastPathPaletteUpdate {
        entries: vec![
            PaletteEntry {
                red: 0xFF,
                green: 0x00,
                blue: 0x00,
            },
            PaletteEntry {
                red: 0x00,
                green: 0xFF,
                blue: 0x00,
            },
        ],
    };

    let mut buffer = vec![0u8; palette.size()];
    let mut cursor = WriteCursor::new(&mut buffer);
    palette.encode(&mut cursor).unwrap();

    assert_eq!(buffer[0..2], [0x02, 0x00]); // UPDATETYPE_PALETTE
    assert_eq!(buffer[2..4], [0x00, 0x00]); // padding
    assert_eq!(buffer[4..8], [0x02, 0x00, 0x00, 0x00]); // numberColors = 2
    assert_eq!(buffer[8..11], [0xFF, 0x00, 0x00]); // Red
    assert_eq!(buffer[11..14], [0x00, 0xFF, 0x00]); // Green
}

#[test]
fn fast_path_palette_roundtrip() {
    use ironrdp_core::{ReadCursor, WriteCursor};

    let original = FastPathPaletteUpdate {
        entries: vec![
            PaletteEntry {
                red: 100,
                green: 150,
                blue: 200,
            },
            PaletteEntry {
                red: 50,
                green: 75,
                blue: 25,
            },
            PaletteEntry {
                red: 0,
                green: 0,
                blue: 0,
            },
        ],
    };

    let mut buffer = vec![0u8; original.size()];
    let mut cursor = WriteCursor::new(&mut buffer);
    original.encode(&mut cursor).unwrap();

    let mut cursor = ReadCursor::new(&buffer);
    let decoded = FastPathPaletteUpdate::decode(&mut cursor).unwrap();

    assert_eq!(original.entries.len(), decoded.entries.len());
    for (orig, dec) in original.entries.iter().zip(decoded.entries.iter()) {
        assert_eq!(orig, dec);
    }
}

#[test]
fn decode_fast_path_synchronize() {
    // Synchronize has no data
    let data: [u8; 0] = [];
    let update = FastPathUpdate::decode_with_code(&data, UpdateCode::Synchronize).unwrap();
    assert!(matches!(update, FastPathUpdate::Synchronize));
}

#[test]
fn fast_path_update_code_palette() {
    let palette = FastPathUpdate::Palette(FastPathPaletteUpdate { entries: vec![] });
    assert_eq!(UpdateCode::from(&palette), UpdateCode::Palette);
}

#[test]
fn fast_path_update_code_synchronize() {
    let sync = FastPathUpdate::Synchronize;
    assert_eq!(UpdateCode::from(&sync), UpdateCode::Synchronize);
}

#[test]
fn decode_orders_returns_unsupported_error() {
    let data: [u8; 0] = [];
    let result = FastPathUpdate::decode_with_code(&data, UpdateCode::Orders);
    assert!(result.is_err());
}
