# keep

`keep` is a project-aware process supervisor for local development.

It is a single binary: the foreground supervisor is built into `keep`, with no
daemon, tmux, OpenSSL, or external process-manager dependency.

The product specification and implementation roadmap live in [docs](docs/README.md).

Native configurations live in `~/.config/keep/*.yaml`. From a configured
project, start the foreground supervisor with:

```bash
keep start
```

Running projects can then be controlled from any directory:

```bash
keep ls
keep status shop/api
keep restart shop/api
keep stop shop
```

```bash
just check
cargo run -- config validate --all
```
