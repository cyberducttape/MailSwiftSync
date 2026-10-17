use std::{
    io::{Cursor, Read},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const MAX_IMAP_LIST_INVENTORY_BYTES: usize = 32 * 1024 * 1024;

pub struct MessageFetchBudget<'a> {
    pub deadline: Instant,
    pub cancel: &'a AtomicBool,
}

impl MessageFetchBudget<'_> {
    pub fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err("fuzz stream cancelled".into());
        }
        if Instant::now() >= self.deadline {
            return Err("fuzz stream deadline exceeded".into());
        }
        Ok(())
    }
}

pub fn read_with_deadline<S: Read>(
    stream: &mut S,
    buffer: &mut [u8],
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> Result<usize, String> {
    if cancel.is_some_and(|token| token.load(Ordering::Relaxed)) {
        return Err("fuzz stream cancelled".into());
    }
    if Instant::now() >= deadline {
        return Err("fuzz stream deadline exceeded".into());
    }
    stream.read(buffer).map_err(|error| error.to_string())
}

#[path = "../../../src/imap_probe/folders.rs"]
#[allow(dead_code)]
pub mod folders;
#[path = "../../../src/imap_probe/list_parser.rs"]
pub mod list_parser;
#[path = "../../../src/imap_probe/literal_framing.rs"]
#[allow(dead_code)]
pub mod literal_framing;

pub fn fuzz_line(line: &str) {
    let _ = list_parser::tokens(line);
    let _ = list_parser::mailbox_name(line);
    let _ = list_parser::delimiter(line);
    let _ = list_parser::has_fixed_fields(line);

    // Sample bounded prefixes so truncated LIST attributes and extended
    // fields exercise the production literal-placement decision.
    let prefix_stride = (line.len() / 32).max(1);
    for end in (0..=line.len()).step_by(prefix_stride) {
        if line.is_char_boundary(end) {
            let _ = list_parser::literal_is_mailbox_name(&line[..end]);
        }
    }
    let _ = list_parser::literal_is_mailbox_name(line);
    let _ = list_parser::literal_is_mailbox_name("");
}

pub fn fuzz_stream(bytes: &[u8]) {
    let mut stream = Cursor::new(bytes);
    let mut buffer = [0_u8; 4096];
    let cancel = AtomicBool::new(false);
    let budget = MessageFetchBudget {
        deadline: Instant::now() + Duration::from_secs(60),
        cancel: &cancel,
    };
    let _ = folders::read_imap_list_response_with_budget(
        &mut stream,
        "a005",
        &mut buffer,
        &budget,
    );
}

pub fn fuzz_literal(payload: &[u8]) {
    // The exact declared length ensures even arbitrary payload bytes reach the
    // production streaming state machine's literal-consumption path.
    let mut framed =
        format!("* LIST (\\HasNoChildren) \"/\" {{{}}}\r\n", payload.len()).into_bytes();
    framed.extend_from_slice(payload);
    framed.extend_from_slice(b" (CHILDINFO (\"SUBSCRIBED\"))\r\na005 OK LIST completed\r\n");
    fuzz_stream(&framed);
}
