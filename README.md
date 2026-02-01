# LNURL Client

A command-line client for Lightning Network (LN) URL protocols. It talks to a local **Core Lightning (CLN)** node via Unix socket RPC and performs HTTP requests to LNURL servers for channel opening, withdrawals, and authentication.

## Features

- **request-channel** — Request an inbound channel from an LNURL service (get params, connect to remote node, call open-channel callback).
- **request-withdraw** — Withdraw sats from a service: create a BOLT11 invoice and submit it via the withdraw callback.
- **request-auth** — Prove ownership of your node by signing a challenge (LNURL-auth style: `/auth-challenge` → sign k1 → `/auth-response` with signature and pubkey).

The server base URL can be given as a full URL or as `host:port` (IPv4 or IPv6).

---

## Requirements

### Runtime

- **Rust** (e.g. 1.70+; install via [rustup](https://rustup.rs/)).
- **Core Lightning (CLN)** — `lightningd` running on the same machine, with a socket at the path used by the client (see [Configuration](#configuration)).
- **Bitcoin** — For LN: a synced `bitcoind` (or compatible backend) on the same network as CLN (e.g. **testnet4** for coursework).

So the minimal setup is: **bitcoind** (testnet4) + **lightningd** (testnet4) + this client. The LNURL **server** runs elsewhere (your own or your professor’s).

### Build

- Dependencies are in `Cargo.toml`; `cargo build` will pull them (e.g. `cln-rpc`, `ureq`, `serde`, `anyhow`, `url`, `secp256k1`, `urlencoding`).

---

## Configuration

| Variable        | Description |
|----------------|-------------|
| `CLN_RPC_PATH` | Path to the Core Lightning RPC socket. If unset, the client uses a default path for testnet4 (e.g. `~/.lightning/testnet4/lightning-rpc`). |

Example (Linux/macOS):

```bash
export CLN_RPC_PATH="$HOME/.lightning/testnet4/lightning-rpc"
```

---

## Build and run

```bash
# Debug build
cargo build
cargo run -- <command> [args...]

# Release build (recommended for real use)
cargo build --release
./target/release/lnurl-client <command> [args...]
```

---

## Usage

### request-channel

Request an inbound channel from an LNURL server. The client fetches channel params, connects your node to the server’s node, then calls the open-channel callback with your pubkey and the challenge.

```bash
lnurl-client request-channel <url|host:port>
```

### request-withdraw

Withdraw sats from an LNURL server. The client gets withdraw params (callback, k1, min/max amount), creates a BOLT11 invoice for the requested amount, then calls the withdraw callback with `k1` and the invoice (`pr`).

```bash
lnurl-client request-withdraw <url|host:port> <amount_msat> [description]
```


### request-auth

Authenticate by signing a challenge. The client calls `/auth-challenge` to get a `k1`, signs it with the local CLN node (`signmessage`), then calls `/auth-response` with `k1`, `signature` (CLN’s `zbase`), and `pubkey`.

```bash
lnurl-client request-auth <url|host:port>
# or
lnurl-client lnurl-auth <url|host:port>
```


---

## Example end-to-end (testnet4)

1. Start **bitcoind** and **lightningd** on testnet4 and fund your node if needed.
2. Point the client at your CLN socket (via `CLN_RPC_PATH` or default).
3. Run commands against an LNURL server (your own or a remote one):

```bash
export CLN_RPC_PATH="$HOME/.lightning/testnet4/lightning-rpc"

./target/release/lnurl-client request-channel https://example.com:3000
./target/release/lnurl-client request-withdraw https://example.com:3000 10000
./target/release/lnurl-client request-auth https://example.com:3000
```

---

## Project layout

```
LN_Client/
├── Cargo.toml
├── README.md
└── src/
    └── main.rs   # CLI, LNURL flows, CLN RPC calls
```

The code is structured in sections: configuration, CLI parsing, Lightning RPC helpers, then one block per command (channel, withdraw, auth) with the relevant types and HTTP calls.

---

## License

See repository or course material for license and authorship.
