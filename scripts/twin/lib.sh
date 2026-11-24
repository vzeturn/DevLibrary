# shellcheck shell=bash disable=SC2034 # sourced by twin.sh and the tests
# Helpers for scripts/twin/*.sh. Secrets (AWS keys, the enrolment code) come from the
# environment and never reach the output, a file in the repo or a file on disk.
TWIN_REGION=${TWIN_REGION:-ap-southeast-1}
TWIN_INSTANCE_TYPE=${TWIN_INSTANCE_TYPE:-t3.large}
# Windows Server 2016 Base: the era of the old SQL Server editions the hotels still run.
TWIN_AMI_PARAM=${TWIN_AMI_PARAM:-/aws/service/ami-windows-latest/Windows_Server-2016-English-Full-Base}
TWIN_TTL_MIN=${TWIN_TTL_MIN:-120}      # the guest shuts itself down (and so terminates) after this
TWIN_SQL_EDITION=${TWIN_SQL_EDITION:-2014} # old SQL Server Express edition to install: 2012 or 2014
DRY_RUN=${DRY_RUN:-0}

die() { echo "twin: $*" >&2; exit 1; }
log() { echo "twin: $*" >&2; }
need() { [ -n "${!1:-}" ] || die "$1 is not set${2:+ ($2)}"; }

# aws_call <aws args...>: the single place that talks to AWS. Dry run prints the call (the
# arguments hold no secret: keys live in the environment) and answers with a placeholder.
aws_call() {
  if [ "$DRY_RUN" = 1 ]; then
    local shown=() a prev=""
    for a in "$@"; do
      if [ "$prev" = "--value" ]; then shown+=("***"); else shown+=("$a"); fi
      prev=$a
    done
    echo "+ aws ${shown[*]}" >&2
    case " $* " in
      *" run-instances "*) echo "i-0dryrun0000000000" ;;
      *" get-parameter "*) echo "ami-0dryrun000000000" ;;
      *" get-command-invocation "*) echo '{"dry_run":true}' ;;
      *" send-command "*) echo "dryrun-command-id" ;;
      *) echo "" ;;
    esac
    return 0
  fi
  aws --region "$TWIN_REGION" --no-cli-pager --output text "$@"
}
