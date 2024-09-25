use quinn::RecvStream;

pub(crate) async fn receive_message_raw(stream: &mut RecvStream) -> Vec<u8> {
    const ONE_GIGABYTE_IN_BYTES: usize = 1073741824;
    stream.read_to_end(ONE_GIGABYTE_IN_BYTES).await.unwrap()
}
