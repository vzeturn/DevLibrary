#!/bin/bash
# The live SQL Server tests of dl-agent-pda-sync, on throwaway SQL Server containers.
#
#   crates/dl-agent-pda-sync/scripts/live-sql.sh [<mssql tag>...]
#
# Tags default to 2022-latest and 2017-latest. For each tag it starts
# mcr.microsoft.com/mssql/server:<tag> on a private docker network and runs the
# PDA_SYNC_TEST_SQL-gated tests in a Linux container of the repo's build image
# (scripts/xbuild/Dockerfile, tagged dl-agent-xbuild:local). The tests run in Linux
# because the agent connects with Encrypt=Required and dl-tls has no macOS backend.
# Everything the script creates (containers, network, target volume) is removed on exit.
# Logs: target/pda-live/<tag>.log. Run it under the machine's heavy-job lock.
set -u
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
NET=dl-pda-net
SQL=dl-sql-pda
RUNNER=dl-pda-test
VOL=dl-pda-target
IMAGE=${XBUILD_IMAGE:-dl-agent-xbuild:local}
TAGS=("$@")
[ ${#TAGS[@]} -eq 0 ] && TAGS=(2022-latest 2017-latest)
LOGS="$ROOT/target/pda-live"
mkdir -p "$LOGS"
cleanup() {
  docker rm -f "$RUNNER" "$SQL" >/dev/null 2>&1
  docker network rm "$NET" >/dev/null 2>&1
  docker volume rm "$VOL" >/dev/null 2>&1
}
trap cleanup EXIT
cleanup
docker network create "$NET" >/dev/null || exit 2
docker volume create "$VOL" >/dev/null || exit 2
FINAL=0
for TAG in "${TAGS[@]}"; do
  PW="Pda$(openssl rand -hex 12)!a"
  LOG="$LOGS/$TAG.log"
  docker rm -f "$SQL" >/dev/null 2>&1
  docker run -d --name "$SQL" --network "$NET" --platform linux/amd64 -e ACCEPT_EULA=Y \
    -e "MSSQL_SA_PASSWORD=$PW" "mcr.microsoft.com/mssql/server:$TAG" >/dev/null || exit 2
  READY=0
  for _ in $(seq 1 90); do
    if docker logs "$SQL" 2>&1 | grep -q "SQL Server is now ready for client connections"; then
      READY=1
      break
    fi
    sleep 2
  done
  if [ $READY -ne 1 ]; then
    echo "== $TAG: SQL Server did not start" >&2
    FINAL=2
    continue
  fi
  sleep 5
  ENC=$(python3 -c 'import sys,urllib.parse;print(urllib.parse.quote(sys.argv[1],safe=""))' "$PW")
  docker run --rm --name "$RUNNER" --network "$NET" \
    -v "$ROOT:/src" -v "$VOL:/target" -e CARGO_TARGET_DIR=/target -w /src \
    -e "PDA_SYNC_TEST_SQL=sqlserver://sa:$ENC@$SQL:1433?encrypt=disable" \
    "$IMAGE" cargo test --offline -p dl-agent-pda-sync \
    --test live_smile_pos --test bootstrap_sql_live --test capture_runner_sql \
    --test commands_sql --test staff_sql \
    --test live_orders --test live_orders_combo --test live_orders_tables --no-fail-fast -- --test-threads=1 --nocapture \
    >"$LOG" 2>&1
  RC=$?
  grep -E "^test result|panicked|skipped|SQL Server [0-9]+:" "$LOG"
  echo "== $TAG: exit $RC (log $LOG)"
  [ $RC -ne 0 ] && FINAL=$RC
  docker rm -f "$SQL" >/dev/null 2>&1
done
exit $FINAL
