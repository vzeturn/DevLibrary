#!/usr/bin/env bash
# bash scripts/twin/twin.test.sh : dry-run and always-terminate behaviour, with a fake `aws`.
set -uo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
check() { if [ "$2" = 0 ]; then echo "ok   $1"; else echo "FAIL $1"; fails=$((fails + 1)); fi; }

# A fake aws that logs every call and fails the ones named in FAKE_AWS_FAIL.
mkdir "$tmp/bin"
cat >"$tmp/bin/aws" <<'EOF'
#!/usr/bin/env bash
echo "$*" >>"$FAKE_AWS_LOG"
case " $* " in
  *" run-instances "*) echo i-0fake1 ; exit 0 ;;
  *" get-parameter "*) echo ami-0fake ; exit 0 ;;
  *" send-command "*) [ -n "${FAKE_AWS_FAIL:-}" ] && { echo "boom" >&2; exit 1; }; echo cmd-1; exit 0 ;;
  *" get-command-invocation "*) case " $* " in *Status*) echo Success ;; *) echo '{"ok":true}' ;; esac; exit 0 ;;
  *" describe-instance-information "*) echo Online; exit 0 ;;
esac
exit 0
EOF
chmod +x "$tmp/bin/aws"

# 1. dry-run: prints the AWS calls, never runs aws, still "terminates" at the end, writes JSON.
out=$(cd "$tmp" && PATH="$tmp/bin:$PATH" FAKE_AWS_LOG="$tmp/dry.log" DL_AGENT_ENROLL_CODE=s3cr3t-code TWIN_HUB=https://hub.invalid \
  bash "$here/twin.sh" --dry-run --out "$tmp/dry.json" 2>&1)
rc=$?
check "dry-run exits 0" $((rc != 0))
[ ! -s "$tmp/dry.log" ]; check "dry-run never runs aws" $?
echo "$out" | grep -q '+ aws ec2 run-instances'; check "dry-run prints run-instances" $?
echo "$out" | grep -q '+ aws ec2 terminate-instances'; check "dry-run prints terminate-instances" $?
echo "$out" | grep -q 's3cr3t-code'; [ $? -ne 0 ]; check "dry-run never prints the enrolment code" $?
grep -q '"ok":true' "$tmp/dry.json"; check "dry-run writes the results JSON" $?

# 2. a failing phase (send-command dies): the instance is terminated anyway, exit is non-zero.
(cd "$tmp" && PATH="$tmp/bin:$PATH" FAKE_AWS_LOG="$tmp/fail.log" FAKE_AWS_FAIL=1 \
  AWS_ACCESS_KEY_ID=AKIAFAKE AWS_SECRET_ACCESS_KEY=fakesecret TWIN_SUBNET_ID=subnet-1 TWIN_SECURITY_GROUP_ID=sg-1 \
  TWIN_INSTANCE_PROFILE=p bash "$here/twin.sh" --installer-url https://example.invalid/a.exe --out "$tmp/f.json" >"$tmp/fail.out" 2>&1)
rc=$?
[ $rc -ne 0 ]; check "failing run exits non-zero" $?
grep -q 'terminate-instances --instance-ids i-0fake1' "$tmp/fail.log"; check "failing run still terminates the instance" $?
grep -q 'fakesecret\|AKIAFAKE' "$tmp/fail.out" "$tmp/fail.log"; [ $? -ne 0 ]; check "credentials never printed or passed as args" $?

# 3. a kill (TERM) in the middle of a run still terminates: send-command hangs, we signal the script.
cat >"$tmp/bin/aws2" <<'EOF'
#!/usr/bin/env bash
echo "$*" >>"$FAKE_AWS_LOG"
case " $* " in
  *" run-instances "*) echo i-0fake2 ;;
  *" get-parameter "*) echo ami-0fake ;;
  *" describe-instance-information "*) echo Online ;;
  *" send-command "*) sleep 2 ;;
esac
exit 0
EOF
chmod +x "$tmp/bin/aws2"; mv "$tmp/bin/aws2" "$tmp/bin/aws"
(cd "$tmp" || exit 1
  PATH="$tmp/bin:$PATH" FAKE_AWS_LOG="$tmp/kill.log" AWS_ACCESS_KEY_ID=a AWS_SECRET_ACCESS_KEY=b \
  TWIN_SUBNET_ID=s TWIN_SECURITY_GROUP_ID=g TWIN_INSTANCE_PROFILE=p \
  bash "$here/twin.sh" --installer-url https://example.invalid/a.exe --out "$tmp/k.json" >/dev/null 2>&1 &
  pid=$!; for _ in $(seq 1 50); do grep -q send-command "$tmp/kill.log" 2>/dev/null && break; sleep 0.1; done
  kill -TERM "$pid"; wait "$pid" 2>/dev/null)
grep -q 'terminate-instances --instance-ids i-0fake2' "$tmp/kill.log"; check "TERM mid-run still terminates the instance" $?

exit $((fails > 0))
