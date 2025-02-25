use quinn::{ReadToEndError, RecvStream};

pub(crate) async fn receive_message_raw(
    stream: &mut RecvStream,
) -> Result<Vec<u8>, ReadToEndError> {
    const ONE_GIGABYTE_IN_BYTES: usize = 1073741824;
    stream.read_to_end(ONE_GIGABYTE_IN_BYTES).await
}
