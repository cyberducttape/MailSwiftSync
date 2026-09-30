# IMAP parser fuzzing

The standalone `fuzz/` package uses cargo-fuzz/libFuzzer against the same
parser source files used by the application. It is deliberately outside the
desktop application's dependency graph.

Targets cover:

- `list_response`: LIST tokenization, quoted mailbox names, and delimiters;
- `fetch_response`: metadata FETCH records, Message-ID literals, and bounded
  body-literal fingerprints;
- `literal_framing`: tagged completion detection with arbitrary and
  chunk-split literal contents;
- `namespace_response`: RFC 2342 namespace response parsing.

The checked-in corpora contain representative valid and malformed seeds.
GitHub Actions runs short campaigns on pull requests, longer campaigns on
`main`, and a daily campaign for every target. The evolved corpus is uploaded
as a workflow artifact so minimized regressions can be added to the permanent
seed set.

Install Rust nightly, a C++ compiler, and cargo-fuzz 0.13.2, then run a target
locally:

```sh
cargo +nightly fuzz run --release fetch_response -- -max_total_time=600 -timeout=10
```

Repeat with `list_response`, `literal_framing`, and `namespace_response`.
Crashes are written below `fuzz/artifacts/<target>/`; preserve each minimized
crashing input as a regression fixture before changing parser behavior.

Fuzzing validates parser robustness; it does not establish interoperability
with a provider. Live differential qualification remains tracked separately
in [the compatibility matrix](compatibility-matrix.md). At present the hosted
Gmail/Workspace, Exchange Online, and Fastmail routes have no live evidence;
the fuzz campaign must not be presented as provider qualification.
