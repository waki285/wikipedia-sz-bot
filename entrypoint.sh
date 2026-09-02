#!/bin/sh
set -e

# mwbot refuses config files readable by other users (must be 600).
# Bind mounts preserve host permissions, so copy the mounted config to a
# private writable location owned by the container user.
if [ -f /app/mwbot.toml ]; then
    cp /app/mwbot.toml /data/mwbot.toml
    chmod 600 /data/mwbot.toml
fi

exec wikipedia_sz_bot "$@"