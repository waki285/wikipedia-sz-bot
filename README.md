# wikipedia_sz_bot

A maintenance bot for the Japanese Wikipedia, built with the
[mwbot](https://crates.io/crates/mwbot) framework.

Every other day, the bot collects the most-transcluded templates that have no
`TemplateData` and saves the top 50 to:

```
利用者:SzBot/メンテナンス/多数使用されているTemplateDataがないテンプレート
```

The list is obtained from the `Mostlinkedtemplates` special page (ranked by
transclusion count) and filtered using the `templatedata` API.

## Setup

1. Copy `mwbot.example.toml` to `mwbot.toml`:

   ```bash
   cp mwbot.example.toml mwbot.toml
   ```

2. Edit `mwbot.toml` and set the bot credentials. Create an owner-only OAuth2
   consumer at
   [Special:OAuthConsumerRegistration](https://meta.wikimedia.org/wiki/Special:OAuthConsumerRegistration)
   and fill in the `username` and `oauth2_token` fields.

   The file is git-ignored because it contains a secret.

## Running

Build and run the application:

```bash
cargo run
```

The bot runs in a built-in loop and updates the page every 48 hours.

To preview the generated wikitext without saving, use:

```bash
cargo run -- --dry-run
```

## Triggering tasks over HTTP

The bot also starts an HTTP server that runs a task immediately when a POST
request is made:

```bash
curl -X POST http://localhost:8080/run/templatedata
```

The endpoint path selects the task:

- `POST /run/templatedata` - run the template data maintenance task

The listening port is set with the `SZ_BOT_PORT` environment variable and
defaults to `8080`:

```bash
SZ_BOT_PORT=9000 cargo run
```

## Running with Docker

Build and run the container. The configuration file is mounted read-only so
credentials stay out of the image:

```bash
# Build (dependencies are cached in a layer, so rebuilds are fast)
docker build -t wikipedia-sz-bot .

# Run, mounting mwbot.toml and publishing the port
docker run -d \
  -p 8080:8080 \
  -v "$(pwd)/mwbot.toml:/app/mwbot.toml:ro" \
  wikipedia-sz-bot
```

Or use docker compose:

```bash
docker compose up -d
```

The `SZ_BOT_PORT` and `RUST_LOG` environment variables are honoured, e.g.
`docker run -e SZ_BOT_PORT=9000 -p 9000:9000 ...`.

### Build caching

The Dockerfile uses a multi-stage build with a cached dependency layer, so
full rebuilds from cached layers take only seconds. The cargo registry is
additionally kept in a BuildKit cache mount to avoid re-downloading crates.

## Publishing to GHCR

Trigger the `Publish image` workflow manually (workflow_dispatch) to build a
multi-architecture image (linux/amd64 and linux/arm64) and push it to the
GitHub Container Registry:

```bash
gh workflow run publish.yml
```

The image is pushed to `ghcr.io/waki285/wikipedia-sz-bot` with two tags:

- `ghcr.io/waki285/wikipedia-sz-bot:<sha>` - the commit the workflow ran on
- `ghcr.io/waki285/wikipedia-sz-bot:latest`

### Architecture

Each architecture is built natively on its own GitHub-hosted runner
(`ubuntu-24.04` for amd64, `ubuntu-24.04-arm` for arm64), so no QEMU
emulation is needed. The per-arch images (`<sha>-amd64`, `<sha>-arm64`,
`latest-amd64`, `latest-arm64`) are then merged into a single
multi-architecture manifest, so one tag works on both amd64 and arm64 hosts:

```bash
docker pull ghcr.io/waki285/wikipedia-sz-bot:latest
docker run -d \
  -p 8080:8080 \
  -v "$(pwd)/mwbot.toml:/app/mwbot.toml:ro" \
  ghcr.io/waki285/wikipedia-sz-bot:latest
```

## Development

Useful commands during development:

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## License

Licensed under either of the following, at your option:

- Apache License, Version 2.0
- MIT License