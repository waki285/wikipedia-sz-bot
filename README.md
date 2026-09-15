# wikipedia_sz_bot

A maintenance bot for the Japanese Wikipedia, built with the
[mwbot](https://crates.io/crates/mwbot) framework.

## Tasks

### `templatedata`

Every other day, collects the most-transcluded templates that have no
`TemplateData` and saves the top 50 to:

```
利用者:SzBot/メンテナンス/多数使用されているTemplateDataがないテンプレート
```

The list is obtained from the `Mostlinkedtemplates` special page (ranked by
transclusion count) and filtered using the `templatedata` API. Templates are
ranked by direct transclusion count in the main namespace (article namespace),
not by total transclusion count.

### `recentchanges`

Runs continuously, polling recent changes and keeping edits by low-edit-count
users whose diff adds a URL with an `utm_source` tracking parameter. The
accumulated list is saved every two hours to:

```
利用者:SzBot/メンテナンス/検知した編集
```

### `ill`

Once a week, scans every main-namespace article that transcludes
`Template:仮リンク` and reports interlanguage links that could be replaced with
a plain wikilink, saving the first 1000 findings to:

```
利用者:SzBot/メンテナンス/日本語版記事が存在する仮リンク
```

A call is reported when the Japanese article name in its first parameter is
missing or a redirect, while the Wikidata item of the linked foreign article
does carry a Japanese sitelink. A redirect that already resolves to exactly
that Japanese article is not reported, since the link points at the right
subject and those calls are tracked by a template category.

Redirects to `Template:仮リンク` (`Ill`, `Ill2`, `Illm`, `Link-interwiki`,
`Interlanguage link` and `Interlanguage link multi`) are recognised as well:
`MediaWiki` records both the redirect and its target in `templatelinks`, so
listing transclusions of the target finds every call.

The scan covers roughly 300,000 articles and issues tens of thousands of API
requests, which is why it runs weekly rather than daily. Wikidata is queried
with the bot's own credentials, since authenticated requests are rate-limited
far less aggressively; if those credentials are not accepted there, the scan
falls back to anonymous access and logs a warning.

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

The bot runs in a built-in loop, running each task on its own schedule.

To preview the generated wikitext without saving, use:

```bash
cargo run -- --dry-run
```

A dry run performs a single pass of every task and prints the result, so it
runs the full `ill` scan unless the scan is capped as shown below.

Two environment variables shrink the work for a quick dry run:

```bash
# Templates scanned as candidates by `templatedata` (default 1000)
SZ_BOT_CANDIDATES=15 cargo run -- --dry-run

# Articles scanned by `ill` (default: no limit)
SZ_BOT_ILL_PAGES=200 cargo run -- --dry-run
```

## Following an `ill` scan

A scan takes hours, so it logs its progress every five minutes at `INFO`:

```
Scanned 50,000/297,792 articles (16%), 1,234 findings, 42m00s elapsed, ~3h28m left
```

When requests are answered far more slowly than a round trip should take, the
scan warns that it is being rate-limited, which is the usual reason a run takes
much longer than expected:

```
jawiki: 8.3s per request, far above a normal round trip. The API is
rate-limiting the bot, so this scan will take much longer than usual.
```

Set `RUST_LOG=wikipedia_sz_bot=debug` for one line per 500-article round with
the lookup and request counts behind those timings:

```
round: 1100 calls, 1048 titles in 21 requests (174.4s), 1084 sitelinks in 34 requests (181.2s)
```

Note that listing the articles to scan happens before the first progress line,
and logs nothing while it pages through the transclusion list.

## Triggering tasks over HTTP

The bot also starts an HTTP server that runs a task immediately when a POST
request is made:

```bash
curl -X POST http://localhost:8080/run/templatedata
```

The endpoint path selects the task:

- `POST /run/templatedata` - run the template data maintenance task
- `POST /run/ill` - run the interlanguage link scan

Resident tasks such as `recentchanges` cannot be triggered this way; the
request is rejected with `400`. The request is held open until the task
finishes, so triggering `ill` keeps the connection open for the whole scan.

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

The container copies the mounted `mwbot.toml` to a private location and
tightens its permissions to `600` at startup, so the host file does not need
special permissions. However, the mount itself must be read-only as shown
above.

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