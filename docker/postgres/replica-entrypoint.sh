#!/bin/bash
# Test standby: clone the primary with pg_basebackup on first start, then run as hot standby.
set -e
until pg_isready -h pg-primary -U dbc -d shop -q; do sleep 1; done
if [ ! -s "$PGDATA/PG_VERSION" ]; then
  PGPASSWORD=replpass pg_basebackup -h pg-primary -U repl -D "$PGDATA" -R -X stream -c fast
  chmod 700 "$PGDATA"
fi
exec postgres -c hot_standby=on -c shared_buffers=64MB
