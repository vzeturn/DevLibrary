# Digital twin test (RT-1054, part A)

`scripts/twin/twin.sh` proves a pre-release dl-agent installer on a machine that looks like a
hotel server, without touching a customer: a one-off EC2 Windows Server 2016 instance with an old
SQL Server Express (2012 or 2014, instance `TWIN`) and a synthetic SMILE sample database
(`scripts/twin/guest/sample_smile.sql`, invented rows, no customer data).

## Phases

1. `sql`: install SQL Server Express (Windows authentication) and load the sample DB. No SQL login is
   created or changed; the `smile` / `smilepms` passwords are never touched.
2. `install`: install the pre-release `dl-agent.exe` (or, with `TWIN_BASE_INSTALLER_URL`, the older
   build first, so the next phase is a real upgrade).
3. `upgrade`: run the pre-release installer over the existing install.
4. `selftest`: `dl-agent selftest --json --no-upload`.
5. `uninstall`: `dl-agent uninstall --purge`; fails if the service is still registered. It runs even
   after an earlier failure.

Each phase's JSON (`{phase, ok, log[]}`) is collected into one file (`--out`, default
`twin-results.json`): `{run, instance, sql_edition, ok, phases:[{phase, ssm_status, result}]}`.
Exit code: 0 all green, 1 a phase failed, 3 the instance could not be terminated.

## Run

```
export AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=...   # from cloud_secrets, never printed
export TWIN_SUBNET_ID=subnet-... TWIN_SECURITY_GROUP_ID=sg-... TWIN_INSTANCE_PROFILE=<role with AmazonSSMManagedInstanceCore + ssm:GetParameter on /twin/*>
scripts/twin/twin.sh --installer-url https://<presigned dl-agent.exe>      # real run
scripts/twin/twin.sh --dry-run                                              # prints the AWS calls, runs none
```

Optional: `TWIN_HUB` + `DL_AGENT_ENROLL_CODE` enrol against a hub (the code travels as an SSM
SecureString parameter that is deleted at exit; in `--dry-run` it is masked). `TWIN_REGION`,
`TWIN_INSTANCE_TYPE`, `TWIN_AMI_PARAM`, `TWIN_TTL_MIN`, `TWIN_SQL_EDITION` are in `lib.sh`.
The security group needs no inbound rule: commands go through SSM, not RDP/WinRM.

## Cleanup guarantees

- `trap cleanup EXIT` (plus INT and TERM) terminates the instance on success, failure, `die`, or a
  signal. If `run-instances` succeeded but the id was not read, it finds the instance by tag `twin-run`.
- The instance is launched with shutdown behaviour `terminate` and a user-data `shutdown` after
  `TWIN_TTL_MIN` (default 120), so a killed harness (SIGKILL, lost machine) still ends the instance.
- If termination itself fails the script says so loudly, with the tag to search, and exits 3.
  Check `aws ec2 describe-instances --filters Name=tag:purpose,Values=dl-agent-twin` after any odd run.

## Test

`bash scripts/twin/twin.test.sh` uses a fake `aws` on PATH: dry-run prints the calls and runs none,
masks the enrolment code, a failing phase and a TERM mid-run both still terminate the instance, and
credentials never appear in output or arguments.

Not covered: the guest PowerShell and the live AWS calls have not been run against real AWS; the
first real run is the acceptance test (SQL Express download URLs may need updating).
