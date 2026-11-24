#!/usr/bin/env bash
# Digital-twin test: a one-off EC2 Windows instance with an old SQL Server Express and a
# synthetic SMILE sample DB, on which the pre-release dl-agent installer is installed, upgraded,
# self-tested and uninstalled. The instance is ALWAYS terminated (trap EXIT), also on failure.
#
#   scripts/twin/twin.sh [--dry-run] [--out results.json] [--installer-url <https url>]
#
# Environment (nothing here is ever printed or written to a file):
#   AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY   credentials (not needed with --dry-run)
#   TWIN_SUBNET_ID, TWIN_SECURITY_GROUP_ID, TWIN_INSTANCE_PROFILE   network + SSM instance role
#   TWIN_INSTALLER_URL        pre-release dl-agent.exe (https, e.g. a presigned URL), or --installer-url
#   TWIN_BASE_INSTALLER_URL   optional older dl-agent.exe installed first so the pre-release is a real upgrade
#   TWIN_HUB, DL_AGENT_ENROLL_CODE   optional: enrol against this hub (code goes through SSM Parameter Store)
#   TWIN_REGION TWIN_INSTANCE_TYPE TWIN_AMI_PARAM TWIN_TTL_MIN TWIN_SQL_EDITION   see lib.sh
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib.sh
. "$here/lib.sh"

OUT=${TWIN_OUT:-twin-results.json}
INSTALLER_URL=${TWIN_INSTALLER_URL:-}
while [ $# -gt 0 ]; do
  case $1 in
    --dry-run) DRY_RUN=1 ;;
    --out) OUT=${2:?--out needs a file}; shift ;;
    --installer-url) INSTALLER_URL=${2:?--installer-url needs a url}; shift ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
  shift
done

RUN_ID="twin-$(date -u +%Y%m%dT%H%M%SZ)-$$"
INSTANCE_ID=""
PARAM_NAME=""
WORK=$(mktemp -d)
RESULTS=()

# Runs on every exit, success or failure: the instance (and the enrolment parameter) go away.
cleanup() {
  local rc=$? fail=0 ids
  trap - EXIT INT TERM
  set +e
  ids=$INSTANCE_ID
  # run-instances may have succeeded without us reading the id: find it by its run tag.
  if [ -z "$ids" ] && [ "$DRY_RUN" != 1 ]; then
    ids=$(aws_call ec2 describe-instances --filters "Name=tag:twin-run,Values=$RUN_ID" \
      "Name=instance-state-name,Values=pending,running,stopping,stopped" \
      --query 'Reservations[].Instances[].InstanceId')
  fi
  if [ -n "$ids" ]; then
    log "terminating $ids"
    # shellcheck disable=SC2086 # several ids are word-split on purpose
    if aws_call ec2 terminate-instances --instance-ids $ids >/dev/null &&
       aws_call ec2 wait instance-terminated --instance-ids $ids; then
      log "terminated $ids"
    else
      log "FAILED to terminate $ids: terminate it by hand (tag twin-run=$RUN_ID); the guest also shuts itself down after ${TWIN_TTL_MIN} min"
      fail=1
    fi
  fi
  if [ -n "$PARAM_NAME" ]; then
    aws_call ssm delete-parameter --name "$PARAM_NAME" >/dev/null || log "could not delete $PARAM_NAME"
  fi
  rm -rf "$WORK"
  if [ "$rc" -eq 0 ] && [ "$fail" -eq 1 ]; then rc=3; fi
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ssm_run <params-file>: sends the commands, waits, prints the command id.
ssm_run() {
  local cid
  cid=$(aws_call ssm send-command --instance-ids "$INSTANCE_ID" --document-name AWS-RunPowerShellScript \
    --parameters "file://$1" --timeout-seconds 1800 --query 'Command.CommandId')
  if [ "$DRY_RUN" != 1 ]; then
    aws_call ssm wait command-executed --command-id "$cid" --instance-id "$INSTANCE_ID" || true
  fi
  echo "$cid"
}

# send <phase> [phase args]: runs guest.ps1 on the instance through SSM and records its JSON.
send() {
  local phase=$1 args=${2:-} cid status out
  printf '{"commands":["powershell -NoProfile -ExecutionPolicy Bypass -File C:\\\\twin\\\\guest.ps1 -Phase %s %s"]}' \
    "$phase" "$args" >"$WORK/params.json"
  cid=$(ssm_run "$WORK/params.json")
  status=$(aws_call ssm get-command-invocation --command-id "$cid" --instance-id "$INSTANCE_ID" --query Status)
  out=$(aws_call ssm get-command-invocation --command-id "$cid" --instance-id "$INSTANCE_ID" --query StandardOutputContent)
  if [ "$DRY_RUN" = 1 ]; then status=Success; fi
  [ -n "$out" ] || out="{}"
  RESULTS+=("{\"phase\":\"$phase\",\"ssm_status\":\"$status\",\"result\":$out}")
  [ "$status" = Success ]
}

# push_guest: copies guest.ps1 and the sample SQL to C:\twin on the instance (base64 over SSM).
push_guest() {
  local g s
  g=$(base64 -w0 "$here/guest/guest.ps1"); s=$(base64 -w0 "$here/guest/sample_smile.sql")
  printf '{"commands":["New-Item -ItemType Directory -Force C:\\\\twin | Out-Null","[IO.File]::WriteAllBytes(\\"C:\\\\twin\\\\guest.ps1\\",[Convert]::FromBase64String(\\"%s\\"))","[IO.File]::WriteAllBytes(\\"C:\\\\twin\\\\sample_smile.sql\\",[Convert]::FromBase64String(\\"%s\\"))"]}' \
    "$g" "$s" >"$WORK/push.json"
  ssm_run "$WORK/push.json" >/dev/null
}

if [ "$DRY_RUN" != 1 ]; then
  need AWS_ACCESS_KEY_ID; need AWS_SECRET_ACCESS_KEY
  need TWIN_SUBNET_ID; need TWIN_SECURITY_GROUP_ID; need TWIN_INSTANCE_PROFILE
  command -v aws >/dev/null || die "the AWS CLI is not installed"
fi
: "${TWIN_SUBNET_ID:=subnet-dryrun}" "${TWIN_SECURITY_GROUP_ID:=sg-dryrun}" "${TWIN_INSTANCE_PROFILE:=twin-ssm}"
if [ -z "$INSTALLER_URL" ]; then
  if [ "$DRY_RUN" = 1 ]; then INSTALLER_URL="https://example.invalid/dl-agent.exe"
  else die "no installer: pass --installer-url or set TWIN_INSTALLER_URL"; fi
fi
case $INSTALLER_URL in https://*) ;; *) die "the installer url must be https" ;; esac

# The guest powers itself off after the TTL; shutdown behaviour terminate makes that a terminate.
printf '<powershell>shutdown /s /t %s</powershell>\n' "$((TWIN_TTL_MIN * 60))" >"$WORK/userdata.txt"

ami=$(aws_call ssm get-parameter --name "$TWIN_AMI_PARAM" --query Parameter.Value)
log "launching $TWIN_INSTANCE_TYPE ($RUN_ID)"
INSTANCE_ID=$(aws_call ec2 run-instances --image-id "$ami" --instance-type "$TWIN_INSTANCE_TYPE" \
  --subnet-id "$TWIN_SUBNET_ID" --security-group-ids "$TWIN_SECURITY_GROUP_ID" \
  --iam-instance-profile "Name=$TWIN_INSTANCE_PROFILE" --count 1 \
  --instance-initiated-shutdown-behavior terminate --metadata-options HttpTokens=required \
  --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=60,VolumeType=gp3,DeleteOnTermination=true}' \
  --user-data "file://$WORK/userdata.txt" \
  --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$RUN_ID},{Key=twin-run,Value=$RUN_ID},{Key=purpose,Value=dl-agent-twin}]" \
  --query 'Instances[0].InstanceId')
[ -n "$INSTANCE_ID" ] || die "run-instances returned no instance id"

aws_call ec2 wait instance-status-ok --instance-ids "$INSTANCE_ID"
# SSM agent registration lags the status check: poll until the instance answers.
if [ "$DRY_RUN" = 1 ]; then
  aws_call ssm describe-instance-information --filters "Key=InstanceIds,Values=$INSTANCE_ID" >/dev/null
else
  online=0
  for _ in $(seq 1 30); do
    ping=$(aws_call ssm describe-instance-information --filters "Key=InstanceIds,Values=$INSTANCE_ID" \
      --query 'InstanceInformationList[0].PingStatus')
    if [ "$ping" = Online ]; then online=1; break; fi
    sleep 10
  done
  [ $online = 1 ] || die "the instance never registered with SSM"
fi

hub_args=""
if [ -n "${TWIN_HUB:-}" ] && [ -n "${DL_AGENT_ENROLL_CODE:-}" ]; then
  PARAM_NAME="/twin/$RUN_ID/enroll-code"
  aws_call ssm put-parameter --name "$PARAM_NAME" --type SecureString --value "$DL_AGENT_ENROLL_CODE" >/dev/null
  hub_args="-Hub $TWIN_HUB -CodeParam $PARAM_NAME"
fi

ok=1
push_guest
send sql "-SqlEdition $TWIN_SQL_EDITION" || ok=0
if [ $ok = 1 ]; then
  send install "-InstallerUrl $INSTALLER_URL ${TWIN_BASE_INSTALLER_URL:+-BaseInstallerUrl $TWIN_BASE_INSTALLER_URL} $hub_args" || ok=0
fi
if [ $ok = 1 ]; then send upgrade "-InstallerUrl $INSTALLER_URL $hub_args" || ok=0; fi
if [ $ok = 1 ]; then send selftest || ok=0; fi
# Uninstall runs even after a failure: it shows whether a broken install can still be removed.
send uninstall || ok=0

{
  printf '{"run":"%s","instance":"%s","sql_edition":"%s","ok":%s,"phases":[' \
    "$RUN_ID" "$INSTANCE_ID" "$TWIN_SQL_EDITION" "$([ $ok = 1 ] && echo true || echo false)"
  sep=""
  for r in "${RESULTS[@]}"; do printf '%s%s' "$sep" "$r"; sep=","; done
  printf ']}\n'
} >"$OUT"
log "results in $OUT"
[ $ok = 1 ] || exit 1
