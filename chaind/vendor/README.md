# Vendored libraries (MIT)

From Martin Seeger's Arcade V1.0, https://github.com/martinseeger2002/arcade-v1 (MIT license, see LICENSE files),
copied unchanged except for Cargo.toml workspace fields:

- `arcade-chain/`: chain primitives (headers, blocks, transactions, AuxPoW, PoW, P2P messages, chain parameters).
- `bitcoinconsensus/`: libbitcoinconsensus (Bitcoin Core 26) with Arcade's per-thread sighash-prefix patch.

`src/rules.rs` of chaind is adapted from arcade-chaindb/src/rules.rs (same license).
