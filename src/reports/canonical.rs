//! Streamed canonical JSON for proof verification and signing.
//!
//! Proof digests and signatures cover the compact, key-sorted JSON that
//! `serde_json::to_string(&Value)` produces. Parsing a fleet-scale proof into a
//! `Value` costs several times the file size in memory, so this module reads a
//! proof once, canonicalizes each top-level member as it arrives, and keeps
//! large members (the mailbox and run arrays) in a private spill file. Nested
//! objects are canonicalized in memory one at a time: they are single mailbox
//! or run entries. The bytes produced are identical to the in-memory path, so
//! existing digests and signatures verify unchanged.

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Members larger than this move from memory to the private spill file.
const SPILL_THRESHOLD: usize = if cfg!(test) {
    64 * 1024
} else {
    4 * 1024 * 1024
};
/// Members read back as JSON values (format, digest, signature, claims) are
/// small by construction; refuse to materialize a large one.
const MAX_SMALL_MEMBER_BYTES: u64 = 1024 * 1024;

enum Member {
    Memory(Vec<u8>),
    /// Byte range of the shared spill file.
    Spilled {
        offset: u64,
        len: u64,
    },
}

/// A proof's top-level members, each held as canonical JSON bytes.
pub(crate) struct ProofDocument {
    members: BTreeMap<String, Member>,
    spill: Option<Spill>,
}

struct Spill {
    file: File,
    end: u64,
    _cleanup: crate::credentials::CleanupGuard,
}

impl ProofDocument {
    /// Read and canonicalize a proof file of at most `max_bytes`.
    pub(crate) fn read(path: &Path, max_bytes: u64) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| error.to_string())?;
        let length = file.metadata().map_err(|error| error.to_string())?.len();
        if length > max_bytes {
            return Err(format!(
                "migration proof exceeds the {max_bytes}-byte limit"
            ));
        }
        let reader = io::BufReader::with_capacity(256 * 1024, file.take(max_bytes + 1));
        let mut deserializer = serde_json::Deserializer::from_reader(reader);
        let mut document = Self {
            members: BTreeMap::new(),
            spill: None,
        };
        de::Deserializer::deserialize_map(&mut deserializer, TopLevel(&mut document))
            .map_err(|error| format!("Invalid migration proof JSON: {error}"))?;
        deserializer
            .end()
            .map_err(|error| format!("Invalid migration proof JSON: {error}"))?;
        Ok(document)
    }

    pub(crate) fn contains(&self, key: &str) -> bool {
        self.members.contains_key(key)
    }

    /// Parse one small member back into a JSON value.
    pub(crate) fn value(&self, key: &str) -> Result<Option<serde_json::Value>, String> {
        let Some(member) = self.members.get(key) else {
            return Ok(None);
        };
        if member_len(member) > MAX_SMALL_MEMBER_BYTES {
            return Err(format!("migration proof field {key} is unexpectedly large"));
        }
        let mut bytes = Vec::new();
        self.copy_member(member, &mut bytes)
            .map_err(|error| error.to_string())?;
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn set(&mut self, key: &str, value: &serde_json::Value) {
        let bytes = serde_json::to_vec(value).expect("a JSON value serializes");
        self.members.insert(key.to_owned(), Member::Memory(bytes));
    }

    pub(crate) fn remove(&mut self, key: &str) {
        self.members.remove(key);
    }

    /// SHA-256 over the canonical document without `proof_digest` and
    /// `proof_signature`, as `with_proof_digest` computes it.
    pub(crate) fn digest(&self) -> Result<String, String> {
        let mut hasher = HashWriter::default();
        self.write_canonical(&mut hasher, &["proof_digest", "proof_signature"], None)
            .map_err(|error| error.to_string())?;
        Ok(super::integrity::hex_encode(&sha2::Digest::finalize(
            hasher.0,
        )))
    }

    /// Write the canonical document, omitting `skip` and optionally replacing
    /// one member with already-canonical bytes. Returns the bytes written.
    pub(crate) fn write_canonical(
        &self,
        out: &mut dyn Write,
        skip: &[&str],
        replace: Option<(&str, &[u8])>,
    ) -> io::Result<u64> {
        let mut out = CountingWriter {
            inner: out,
            count: 0,
        };
        let mut keys = self
            .members
            .keys()
            .map(String::as_str)
            .filter(|key| !skip.contains(key))
            .collect::<Vec<_>>();
        if let Some((key, _)) = replace
            && !keys.contains(&key)
        {
            keys.push(key);
            keys.sort_unstable();
        }
        out.write_all(b"{")?;
        for (index, key) in keys.into_iter().enumerate() {
            if index > 0 {
                out.write_all(b",")?;
            }
            serde_json::to_writer(&mut out, key)?;
            out.write_all(b":")?;
            match replace {
                Some((replaced, bytes)) if replaced == key => out.write_all(bytes)?,
                _ => self.copy_member(&self.members[key], &mut out)?,
            }
        }
        out.write_all(b"}")?;
        Ok(out.count)
    }

    fn copy_member(&self, member: &Member, out: &mut dyn Write) -> io::Result<()> {
        match member {
            Member::Memory(bytes) => out.write_all(bytes),
            Member::Spilled { offset, len } => {
                let spill = self
                    .spill
                    .as_ref()
                    .ok_or_else(|| io::Error::other("proof spill file is missing"))?;
                let mut file = &spill.file;
                file.seek(SeekFrom::Start(*offset))?;
                let copied = io::copy(&mut file.take(*len), out)?;
                if copied != *len {
                    return Err(io::Error::other("proof spill file is truncated"));
                }
                Ok(())
            }
        }
    }

    fn store_member(&mut self, key: String, bytes: Vec<u8>, spilled: Option<(u64, u64)>) {
        let member = match spilled {
            Some((offset, len)) => Member::Spilled { offset, len },
            None => Member::Memory(bytes),
        };
        self.members.insert(key, member);
    }

    fn spill(&mut self) -> io::Result<&mut Spill> {
        if self.spill.is_none() {
            let directory =
                crate::credentials::create_secret_directory().map_err(io::Error::other)?;
            let cleanup = crate::credentials::CleanupGuard::new(vec![directory.clone()]);
            let path = directory.join("proof-spill");
            // Create owner-only, then reopen for the read-back passes.
            drop(crate::credentials::open_secret_file(&path)?);
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)?;
            self.spill = Some(Spill {
                file,
                end: 0,
                _cleanup: cleanup,
            });
        }
        Ok(self.spill.as_mut().expect("spill was just created"))
    }
}

fn member_len(member: &Member) -> u64 {
    match member {
        Member::Memory(bytes) => bytes.len() as u64,
        Member::Spilled { len, .. } => *len,
    }
}

/// Buffers one top-level member and moves it to the spill file once it
/// exceeds the threshold.
struct MemberWriter<'a> {
    document: &'a mut ProofDocument,
    buffer: Vec<u8>,
    spilled: Option<(u64, u64)>,
}

impl Write for MemberWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some((_, len)) = &mut self.spilled {
            let spill = self.document.spill()?;
            spill.file.seek(SeekFrom::Start(spill.end))?;
            spill.file.write_all(bytes)?;
            spill.end += bytes.len() as u64;
            *len += bytes.len() as u64;
            return Ok(bytes.len());
        }
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > SPILL_THRESHOLD {
            let spill = self.document.spill()?;
            let offset = spill.end;
            spill.file.seek(SeekFrom::Start(offset))?;
            spill.file.write_all(&self.buffer)?;
            spill.end += self.buffer.len() as u64;
            self.spilled = Some((offset, self.buffer.len() as u64));
            self.buffer = Vec::new();
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct TopLevel<'a>(&'a mut ProofDocument);

impl<'de> Visitor<'de> for TopLevel<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a migration proof JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            let mut writer = MemberWriter {
                document: self.0,
                buffer: Vec::new(),
                spilled: None,
            };
            map.next_value_seed(Canonical(&mut writer))?;
            let MemberWriter {
                buffer, spilled, ..
            } = writer;
            // A repeated key replaces the earlier value, as in a serde_json map.
            self.0.store_member(key, buffer, spilled);
        }
        Ok(())
    }
}

/// Writes the canonical form of whatever value it is given.
struct Canonical<'w>(&'w mut dyn Write);

impl<'de> DeserializeSeed<'de> for Canonical<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

fn write_error<E: de::Error>(error: io::Error) -> E {
    E::custom(format!("could not write canonical proof: {error}"))
}

impl<'de> Visitor<'de> for Canonical<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<(), E> {
        serde_json::to_writer(self.0, &value).map_err(E::custom)
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<(), E> {
        serde_json::to_writer(self.0, &value).map_err(E::custom)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<(), E> {
        serde_json::to_writer(self.0, &value).map_err(E::custom)
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<(), E> {
        // Same formatting as serde_json::Value, which also stores f64.
        serde_json::to_writer(self.0, &serde_json::Value::from(value)).map_err(E::custom)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<(), E> {
        serde_json::to_writer(self.0, value).map_err(E::custom)
    }

    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        self.0.write_all(b"null").map_err(write_error)
    }

    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        self.visit_unit()
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
        self.0.write_all(b"[").map_err(write_error)?;
        let mut first = true;
        while sequence
            .next_element_seed(Element {
                out: &mut *self.0,
                first,
            })?
            .is_some()
        {
            first = false;
        }
        self.0.write_all(b"]").map_err(write_error)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        // Nested objects are single entries; sort their members in memory.
        let mut members = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            let mut value = Vec::new();
            map.next_value_seed(Canonical(&mut value))?;
            members.insert(key, value);
        }
        self.0.write_all(b"{").map_err(write_error)?;
        for (index, (key, value)) in members.iter().enumerate() {
            if index > 0 {
                self.0.write_all(b",").map_err(write_error)?;
            }
            serde_json::to_writer(&mut *self.0, key).map_err(de::Error::custom)?;
            self.0.write_all(b":").map_err(write_error)?;
            self.0.write_all(value).map_err(write_error)?;
        }
        self.0.write_all(b"}").map_err(write_error)
    }
}

/// An array element: writes its separating comma only once it exists.
struct Element<'w> {
    out: &'w mut dyn Write,
    first: bool,
}

impl<'de> DeserializeSeed<'de> for Element<'_> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        if !self.first {
            self.out
                .write_all(b",")
                .map_err(|error| de::Error::custom(error.to_string()))?;
        }
        deserializer.deserialize_any(Canonical(self.out))
    }
}

#[derive(Default)]
pub(crate) struct HashWriter(pub(crate) sha2::Sha256);

impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        sha2::Digest::update(&mut self.0, bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct CountingWriter<'a> {
    inner: &'a mut dyn Write,
    count: u64,
}

impl Write for CountingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.count += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Discards bytes, counting them; sizes a canonical payload before deciding
/// how to sign it.
#[derive(Default)]
pub(crate) struct Sink;

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ProofDocument;

    fn canonical_through_document(text: &str) -> (String, bool) {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let _cleanup = crate::credentials::CleanupGuard::new(vec![directory.clone()]);
        let path = directory.join("proof.json");
        std::fs::write(&path, text).unwrap();
        let document = ProofDocument::read(&path, 1 << 30).unwrap();
        let mut bytes = Vec::new();
        document.write_canonical(&mut bytes, &[], None).unwrap();
        (String::from_utf8(bytes).unwrap(), document.spill.is_some())
    }

    #[test]
    fn streamed_canonical_bytes_match_serde_json_value() {
        let text = r#"{
            "z": [1, -2, 3.5, 1e300, 18446744073709551615, 0.1, true, null],
            "a": {"y": "é \"quoted\" \u0001 \t", "b": [], "a": {}},
            "m": "line\nbreak",
            "a": "duplicate keys keep the last value"
        }"#;
        let expected =
            serde_json::to_string(&serde_json::from_str::<serde_json::Value>(text).unwrap())
                .unwrap();
        assert_eq!(canonical_through_document(text).0, expected);
    }

    #[test]
    fn large_members_spill_to_disk_and_still_match() {
        let entries = (0..5_000)
            .map(|index| serde_json::json!({"id": format!("mailbox-{index}"), "n": index}))
            .collect::<Vec<_>>();
        let value = serde_json::json!({"format": "x", "mailboxes": entries});
        let text = serde_json::to_string_pretty(&value).unwrap();
        let (canonical, spilled) = canonical_through_document(&text);
        assert!(
            spilled,
            "the mailbox array should exceed the spill threshold"
        );
        assert_eq!(canonical, serde_json::to_string(&value).unwrap());
    }

    #[test]
    fn non_object_and_trailing_content_are_rejected() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let _cleanup = crate::credentials::CleanupGuard::new(vec![directory.clone()]);
        let path = directory.join("proof.json");
        for text in ["[1,2]", "{\"a\":1} {\"b\":2}", "{\"a\":"] {
            std::fs::write(&path, text).unwrap();
            assert!(ProofDocument::read(&path, 1 << 20).is_err(), "{text}");
        }
        std::fs::write(&path, "{\"a\":1}").unwrap();
        assert!(ProofDocument::read(&path, 3).is_err());
    }
}
