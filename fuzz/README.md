# X12 fuzz target

The parser limits and envelope checks are unit-tested in the workspace. For a
full fuzzing campaign, install `cargo-fuzz` and run:

```bash
cd fuzz
cargo +nightly fuzz run x12_tokenizer
```

Run it only against synthetic or generated X12 input.
Never fuzz with production remittances or PHI-bearing corpora.
