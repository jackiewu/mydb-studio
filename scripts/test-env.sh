#!/usr/bin/env bash
# Starts a throwaway MariaDB in Docker for the live tests in
# crates/mydb-core/tests/live.rs, then prints the variables to export.
#
#   ./scripts/test-env.sh            # MariaDB 11 on port 33306
#   IMAGE=mysql:8.4 ./scripts/test-env.sh
set -euo pipefail

IMAGE="${IMAGE:-mariadb:11}"
NAME="${NAME:-mydb-studio-test}"
PORT="${PORT:-33306}"

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" -p "$PORT:3306" \
  -e MARIADB_ROOT_PASSWORD=secret -e MYSQL_ROOT_PASSWORD=secret \
  -e MARIADB_DATABASE=demo -e MYSQL_DATABASE=demo \
  "$IMAGE" >/dev/null

echo "waiting for $IMAGE on port $PORT..." >&2
for _ in $(seq 1 60); do
  if docker exec "$NAME" sh -c 'mariadb-admin ping -psecret --silent 2>/dev/null || mysqladmin ping -psecret --silent 2>/dev/null'; then
    break
  fi
  sleep 1
done

cat <<VARS
export MYDB_TEST_HOST=127.0.0.1
export MYDB_TEST_PORT=$PORT
export MYDB_TEST_USER=root
export MYDB_TEST_PASSWORD=secret
# Optional: MYDB_TEST_CA=/path/to/ca.pem for TLS tests,
# MYDB_TEST_SSH_HOST / _PORT / _USER / _KEY or _PASSWORD for tunnel tests.
VARS
