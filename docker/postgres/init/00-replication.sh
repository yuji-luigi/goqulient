#!/bin/bash
# Allow streaming replication for the test replica (runs once at cluster init).
set -e
psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "CREATE ROLE repl WITH REPLICATION LOGIN PASSWORD 'replpass'"
echo "host replication repl all scram-sha-256" >> "$PGDATA/pg_hba.conf"
