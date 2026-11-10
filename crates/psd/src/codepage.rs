//! Legacy (non-Unicode) layer names.

use encoding_rs::Encoding;

/// `bytes` decoded in `enc`, without byte-order-mark handling.
pub(crate) fn decode(bytes: &[u8], enc: &'static Encoding) -> String {
    enc.decode_without_bom_handling(bytes).0.into_owned()
}
