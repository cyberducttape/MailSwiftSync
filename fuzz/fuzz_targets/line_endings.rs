pub fn protocol_crlf(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(data.len());
    for (index, byte) in data.iter().copied().enumerate() {
        if byte == b'\n' && (index == 0 || data[index - 1] != b'\r') {
            output.push(b'\r');
        }
        output.push(byte);
    }
    output
}
